// SPDX-License-Identifier: Apache-2.0
// Copyright Authors of bpfman

use std::{
    fs::remove_file,
    os::unix::prelude::{FromRawFd, IntoRawFd},
    path::Path,
};

use anyhow::anyhow;
use bpfman_api::{
    config::Config, util::directories::RTPATH_BPFMAN_SOCKET, v1::bpfman_server::BpfmanServer,
};
use libsystemd::activation::IsType;
use log::{debug, error, info};
use tokio::{
    join,
    net::UnixListener,
    signal::unix::{signal, SignalKind},
    sync::{broadcast, mpsc},
    task::{JoinHandle, JoinSet},
};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

use crate::{
    bpf::BpfManager,
    oci_utils::ImageManager,
    rpc::BpfmanLoader,
    storage::StorageManager,
    utils::{set_file_permissions, SOCK_MODE},
    ROOT_DB,
};

pub async fn serve(config: &Config, csi_support: bool, timeout: u64) -> anyhow::Result<()> {
    let (shutdown_tx, shutdown_rx1) = broadcast::channel(32);
    let shutdown_rx2 = shutdown_tx.subscribe();
    let shutdown_rx3 = shutdown_tx.subscribe();
    let shutdown_rx4 = shutdown_tx.subscribe();
    let shutdown_handle = tokio::spawn(shutdown_handler(timeout, shutdown_tx));

    let (tx, rx) = mpsc::channel(32);

    let loader = BpfmanLoader::new(tx.clone());
    let path = RTPATH_BPFMAN_SOCKET.to_string();
    let service = BpfmanServer::new(loader);

    let mut listeners: Vec<_> = Vec::new();

    let handle = serve_unix(path.clone(), service.clone(), shutdown_rx1).await?;
    listeners.push(handle);

    let allow_unsigned = config.signing.as_ref().map_or(true, |s| s.allow_unsigned);
    let (itx, irx) = mpsc::channel(32);

    let mut image_manager = ImageManager::new(ROOT_DB.clone(), allow_unsigned, irx).await?;
    let image_manager_handle = tokio::spawn(async move {
        image_manager.run(shutdown_rx2).await;
    });

    let mut bpf_manager = BpfManager::new(config.clone(), rx, itx);
    bpf_manager.rebuild_state().await?;

    // TODO(astoycos) see issue #881
    //let static_programs = get_static_programs(static_program_path).await?;

    // Load any static programs first
    // if !static_programs.is_empty() {
    //     for prog in static_programs {
    //         let ret_prog = bpf_manager.add_program(prog).await?;
    //         // Get the Kernel Info.
    //         let kernel_info = ret_prog
    //             .kernel_info()
    //             .expect("kernel info should be set for all loaded programs");
    //         info!("Loaded static program with program id {}", kernel_info.id)
    //     }
    // };

    if csi_support {
        let storage_manager = StorageManager::new(tx);
        let storage_manager_handle =
            tokio::spawn(async move { storage_manager.run(shutdown_rx3).await });
        let (_, res_image, res_storage, _, _) = join!(
            join_listeners(listeners),
            image_manager_handle,
            storage_manager_handle,
            bpf_manager.process_commands(shutdown_rx4),
            shutdown_handle
        );
        if let Some(e) = res_storage.err() {
            return Err(e.into());
        }
        if let Some(e) = res_image.err() {
            return Err(e.into());
        }
    } else {
        let (_, res_image, _, _) = join!(
            join_listeners(listeners),
            image_manager_handle,
            bpf_manager.process_commands(shutdown_rx4),
            shutdown_handle
        );
        if let Some(e) = res_image.err() {
            return Err(e.into());
        }
    }

    Ok(())
}

pub(crate) async fn shutdown_handler(timeout: u64, shutdown_tx: broadcast::Sender<()>) {
    let mut joinset = JoinSet::new();
    if timeout > 0 {
        info!("Using inactivity timer of {} seconds", timeout);
        let duration: std::time::Duration = std::time::Duration::from_secs(timeout);
        joinset.spawn(Box::pin(tokio::time::sleep(duration)));
    } else {
        info!("Using no inactivity timer");
    }
    let mut sigint = signal(SignalKind::interrupt()).unwrap();
    joinset.spawn(async move {
        sigint.recv().await;
        debug!("Received SIGINT");
    });

    let mut sigterm = signal(SignalKind::terminate()).unwrap();
    joinset.spawn(async move {
        sigterm.recv().await;
        debug!("Received SIGTERM");
    });

    joinset.join_next().await;
    shutdown_tx.send(()).unwrap();
}

async fn join_listeners(listeners: Vec<JoinHandle<()>>) {
    for listener in listeners {
        match listener.await {
            Ok(()) => {}
            Err(e) => eprintln!("Error = {e:?}"),
        }
    }
}

async fn serve_unix(
    path: String,
    service: BpfmanServer<BpfmanLoader>,
    mut shutdown_channel: broadcast::Receiver<()>,
) -> anyhow::Result<JoinHandle<()>> {
    let uds_stream = if let Ok(stream) = systemd_unix_stream(path.clone()) {
        stream
    } else {
        std_unix_stream(path.clone()).await?
    };

    let serve = Server::builder()
        .add_service(service)
        .serve_with_incoming_shutdown(uds_stream, async move {
            match shutdown_channel.recv().await {
                Ok(()) => debug!("Unix Socket: Received shutdown signal"),
                Err(e) => error!("Error receiving shutdown signal {:?}", e),
            };
        });

    Ok(tokio::spawn(async move {
        info!("Listening on {path}");
        if let Err(e) = serve.await {
            eprintln!("Error = {e:?}");
        }
        info!("Shutdown Unix Handler {}", path);
    }))
}

fn systemd_unix_stream(_path: String) -> anyhow::Result<UnixListenerStream> {
    let listen_fds = libsystemd::activation::receive_descriptors(true)?;
    if listen_fds.len() == 1 {
        if let Some(fd) = listen_fds.first() {
            if !fd.is_unix() {
                return Err(anyhow!("Wrong Socket"));
            }
            let std_listener =
                unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd.clone().into_raw_fd()) };
            std_listener.set_nonblocking(true)?;
            let tokio_listener = UnixListener::from_std(std_listener)?;
            info!("Using a Unix socket from systemd");
            return Ok(UnixListenerStream::new(tokio_listener));
        }
    }

    Err(anyhow!("Unable to retrieve fd from systemd"))
}

async fn std_unix_stream(path: String) -> anyhow::Result<UnixListenerStream> {
    // Listen on Unix socket
    if Path::new(&path).exists() {
        // Attempt to remove the socket, since bind fails if it exists
        remove_file(&path)?;
    }

    let uds = UnixListener::bind(&path)?;
    let stream = UnixListenerStream::new(uds);
    // Always set the file permissions of our listening socket.
    set_file_permissions(&path.clone(), SOCK_MODE).await;

    info!("Using default Unix socket");
    Ok(stream)
}
