// SPDX-License-Identifier: Apache-2.0
// Copyright Authors of bpfman

use std::io::{copy, Read};

use bpfman_api::ImagePullPolicy;
use flate2::read::GzDecoder;
use log::{debug, info, trace};
use oci_distribution::{
    client::{ClientConfig, ClientProtocol},
    manifest,
    manifest::OciImageManifest,
    secrets::RegistryAuth,
    Client, Reference,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sled::Db;
use tar::Archive;
use tokio::{
    select,
    sync::{
        broadcast,
        mpsc::{self, Receiver},
        oneshot,
    },
};

use crate::oci_utils::{cosign::CosignVerifier, ImageError};

#[derive(Debug, Deserialize, Default)]
pub struct ContainerImageMetadata {
    #[serde(rename(deserialize = "io.ebpf.program_name"))]
    pub name: String,
    #[serde(rename(deserialize = "io.ebpf.bpf_function_name"))]
    pub bpf_function_name: String,
    #[serde(rename(deserialize = "io.ebpf.program_type"))]
    pub program_type: String,
    #[serde(rename(deserialize = "io.ebpf.filename"))]
    pub filename: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct BytecodeImage {
    pub(crate) image_url: String,
    pub(crate) image_pull_policy: ImagePullPolicy,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
}

impl BytecodeImage {
    pub(crate) fn new(
        image_url: String,
        image_pull_policy: i32,
        username: Option<String>,
        password: Option<String>,
    ) -> Self {
        Self {
            image_url,
            image_pull_policy: image_pull_policy
                .try_into()
                .expect("Unable to parse ImagePullPolicy"),
            username,
            password,
        }
    }

    pub(crate) fn get_url(&self) -> &str {
        &self.image_url
    }

    pub(crate) fn get_pull_policy(&self) -> &ImagePullPolicy {
        &self.image_pull_policy
    }
}

impl From<bpfman_api::v1::BytecodeImage> for BytecodeImage {
    fn from(value: bpfman_api::v1::BytecodeImage) -> Self {
        // This function is mapping an empty string to None for
        // username and password.
        let username = if value.username.is_some() {
            match value.username.unwrap().as_ref() {
                "" => None,
                u => Some(u.to_string()),
            }
        } else {
            None
        };
        let password = if value.password.is_some() {
            match value.password.unwrap().as_ref() {
                "" => None,
                u => Some(u.to_string()),
            }
        } else {
            None
        };
        BytecodeImage::new(value.url, value.image_pull_policy, username, password)
    }
}

pub(crate) struct ImageManager {
    database: Db,
    client: Client,
    cosign_verifier: CosignVerifier,
    rx: Receiver<Command>,
}

/// Provided by the requester and used by the manager task to send
/// the command response back to the requester.
type Responder<T> = oneshot::Sender<T>;

#[derive(Debug)]
pub(crate) enum Command {
    Pull {
        image: String,
        pull_policy: ImagePullPolicy,
        username: Option<String>,
        password: Option<String>,
        resp: Responder<Result<(String, String), ImageError>>,
    },
    GetBytecode {
        path: String,
        resp: Responder<Result<Vec<u8>, ImageError>>,
    },
}

impl ImageManager {
    pub(crate) async fn new(
        database: Db,
        allow_unsigned: bool,
        rx: mpsc::Receiver<Command>,
    ) -> Result<Self, anyhow::Error> {
        let cosign_verifier = CosignVerifier::new(allow_unsigned).await?;
        let config = ClientConfig {
            protocol: ClientProtocol::Https,
            ..Default::default()
        };
        let client = Client::new(config);
        Ok(Self {
            database,
            cosign_verifier,
            client,
            rx,
        })
    }

    pub(crate) async fn run(&mut self, mut shutdown_rx: broadcast::Receiver<()>) {
        loop {
            // Start receiving messages
            select! {
                biased;
                _ = shutdown_rx.recv() => {
                    info!("image_manager: Signal received to stop command processing");
                    self.database.flush().expect("Unable to flush database to disk before shutting down ImageManager");
                    break;
                }
                Some(cmd) = self.rx.recv() => {
                    match cmd {
                        Command::Pull { image, pull_policy, username, password, resp } => {
                            let result = self.get_image(&image, pull_policy, username, password).await;
                            let _ = resp.send(result);
                        },
                        Command::GetBytecode { path, resp } => {
                            let result = self.get_bytecode_from_image_store(path).await;
                            let _ = resp.send(result);
                        }
                    }
                }
            }
        }
        info!("image_manager: Stopped processing commands");
    }

    pub(crate) async fn get_image(
        &mut self,
        image_url: &str,
        pull_policy: ImagePullPolicy,
        username: Option<String>,
        password: Option<String>,
    ) -> Result<(String, String), ImageError> {
        // The reference created here is created using the krustlet oci-distribution
        // crate. It currently contains many defaults more of which can be seen
        // here: https://github.com/krustlet/oci-distribution/blob/main/src/reference.rs#L58
        let image: Reference = image_url.parse().map_err(ImageError::InvalidImageUrl)?;

        self.cosign_verifier
            .verify(image_url, username.as_deref(), password.as_deref())
            .await?;

        let image_content_key = get_image_content_key(&image);

        let exists: bool = self
            .database
            .contains_key(image_content_key.to_string() + "manifest.json")
            .map_err(|e| {
                ImageError::DatabaseError("failed to read db".to_string(), e.to_string())
            })?;

        let image_meta = match pull_policy {
            ImagePullPolicy::Always => {
                self.pull_image(image, &image_content_key, username, password)
                    .await?
            }
            ImagePullPolicy::IfNotPresent => {
                if exists {
                    self.load_image_meta(&image_content_key)?
                } else {
                    self.pull_image(image, &image_content_key, username, password)
                        .await?
                }
            }
            ImagePullPolicy::Never => {
                if exists {
                    self.load_image_meta(&image_content_key)?
                } else {
                    Err(ImageError::ByteCodeImageNotfound(image.to_string()))?
                }
            }
        };

        Ok((image_content_key.to_string(), image_meta.bpf_function_name))
    }

    fn get_auth_for_registry(
        &self,
        _registry: &str,
        username: Option<String>,
        password: Option<String>,
    ) -> RegistryAuth {
        match (username, password) {
            (Some(username), Some(password)) => RegistryAuth::Basic(username, password),
            _ => RegistryAuth::Anonymous,
        }
    }

    pub async fn pull_image(
        &mut self,
        image: Reference,
        base_key: &str,
        username: Option<String>,
        password: Option<String>,
    ) -> Result<ContainerImageMetadata, ImageError> {
        debug!(
            "Pulling bytecode from image path: {}/{}:{}",
            image.registry(),
            image.repository(),
            image.tag().unwrap_or("latest")
        );

        let auth = self.get_auth_for_registry(image.registry(), username, password);

        let (image_manifest, _, config_contents) = self
            .client
            .pull_manifest_and_config(&image.clone(), &auth)
            .await
            .map_err(ImageError::ImageManifestPullFailure)?;

        trace!("Raw container image manifest {}", image_manifest);

        let image_manifest_key = base_key.to_string() + "manifest.json";

        let image_manifest_json = serde_json::to_string(&image_manifest)
            .map_err(|e| ImageError::ByteCodeImageProcessFailure(e.into()))?;

        // inset and flush to disk to avoid races across threads on write.
        self.database
            .insert(image_manifest_key, image_manifest_json.as_str())
            .map_err(|e| {
                ImageError::DatabaseError("failed to write to db".to_string(), e.to_string())
            })?;
        self.database.flush().map_err(|e| {
            ImageError::DatabaseError("failed to flush db".to_string(), e.to_string())
        })?;

        let config_sha = &image_manifest
            .config
            .digest
            .split(':')
            .collect::<Vec<&str>>()[1];

        let image_config_path = base_key.to_string() + config_sha;

        let bytecode_sha = image_manifest.layers[0]
            .digest
            .split(':')
            .collect::<Vec<&str>>()[1];

        let bytecode_path = base_key.to_string() + bytecode_sha;

        let image_config: Value = serde_json::from_str(&config_contents)
            .map_err(|e| ImageError::ByteCodeImageProcessFailure(e.into()))?;
        trace!("Raw container image config {}", image_config);

        // Deserialize image metadata(labels) from json config
        let image_labels: ContainerImageMetadata =
            serde_json::from_str(&image_config["config"]["Labels"].to_string())
                .map_err(|e| ImageError::ByteCodeImageProcessFailure(e.into()))?;

        self.database
            .insert(image_config_path, config_contents.as_str())
            .map_err(|e| {
                ImageError::DatabaseError("failed to write to db".to_string(), e.to_string())
            })?;
        self.database.flush().map_err(|e| {
            ImageError::DatabaseError("failed to flush db".to_string(), e.to_string())
        })?;

        let image_content = self
            .client
            .pull(
                &image,
                &auth,
                vec![
                    manifest::IMAGE_LAYER_GZIP_MEDIA_TYPE,
                    manifest::IMAGE_DOCKER_LAYER_GZIP_MEDIA_TYPE,
                ],
            )
            .await
            .map_err(ImageError::BytecodeImagePullFailure)?
            .layers
            .into_iter()
            .next()
            .map(|layer| layer.data)
            .ok_or(ImageError::BytecodeImageExtractFailure)?;

        self.database
            .insert(bytecode_path, image_content)
            .map_err(|e| {
                ImageError::DatabaseError("failed to write to db".to_string(), e.to_string())
            })?;
        self.database.flush().map_err(|e| {
            ImageError::DatabaseError("failed to flush db".to_string(), e.to_string())
        })?;

        Ok(image_labels)
    }

    pub(crate) async fn get_bytecode_from_image_store(
        &self,
        base_key: String,
    ) -> Result<Vec<u8>, ImageError> {
        let manifest = serde_json::from_str::<OciImageManifest>(
            std::str::from_utf8(
                &self
                    .database
                    .get(base_key.clone() + "manifest.json")
                    .map_err(|e| {
                        ImageError::DatabaseError("failed to read db".to_string(), e.to_string())
                    })?
                    .expect("Image manifest is empty"),
            )
            .unwrap(),
        )
        .map_err(|e| {
            ImageError::DatabaseError(
                "failed to parse image manifest from db".to_string(),
                e.to_string(),
            )
        })?;

        let bytecode_sha = &manifest.layers[0].digest;

        let bytecode_key = base_key + bytecode_sha.clone().split(':').collect::<Vec<&str>>()[1];

        debug!(
            "bytecode is stored as tar+gzip file at key {}",
            bytecode_key
        );

        let f = self
            .database
            .get(bytecode_key.clone())
            .map_err(|e| ImageError::DatabaseError("failed to read db".to_string(), e.to_string()))?
            .ok_or(ImageError::DatabaseError(
                "key does not exist in db".to_string(),
                String::new(),
            ))?;

        let mut hasher = Sha256::new();
        copy(&mut f.as_ref(), &mut hasher).expect("cannot copy bytecode to hasher");
        let hash = hasher.finalize();
        let expected_sha = "sha256:".to_owned() + &base16ct::lower::encode_string(&hash);

        if *bytecode_sha != expected_sha {
            debug!(
                "actual SHA256: {}\nexpected SHA256: {:?}",
                bytecode_sha, expected_sha
            );
            panic!("Bpf Bytecode has been compromised")
        }

        // The data is of OCI media type "application/vnd.oci.image.layer.v1.tar+gzip" or
        // "application/vnd.docker.image.rootfs.diff.tar.gzip"
        // decode and unpack to access bytecode
        let unzipped_tarball = GzDecoder::new(f.as_ref());

        return Ok(Archive::new(unzipped_tarball)
            .entries()
            .expect("unable to parse tarball entries")
            .filter_map(|e| e.ok())
            .map(|mut entry| {
                let mut data = Vec::new();
                entry
                    .read_to_end(&mut data)
                    .expect("unable to read bytecode tarball entry");
                data
            })
            .collect::<Vec<Vec<u8>>>()
            .first()
            .expect("unable to get bytecode file bytes")
            .to_owned());
    }

    fn load_image_meta(
        &self,
        image_content_key: &str,
    ) -> Result<ContainerImageMetadata, anyhow::Error> {
        let manifest = serde_json::from_str::<OciImageManifest>(
            std::str::from_utf8(
                &self
                    .database
                    .get(image_content_key.to_string() + "manifest.json")
                    .map_err(|e| {
                        ImageError::DatabaseError("failed to read db".to_string(), e.to_string())
                    })?
                    .expect("Image manifest is empty"),
            )
            .unwrap(),
        )
        .map_err(|e| {
            ImageError::DatabaseError(
                "failed to parse db entry to image manifest".to_string(),
                e.to_string(),
            )
        })?;

        let config_sha = &manifest.config.digest.split(':').collect::<Vec<&str>>()[1];

        let image_config_key = image_content_key.to_string() + config_sha;

        let db_content = &self
            .database
            .get(image_config_key)
            .map_err(|e| ImageError::DatabaseError("failed to read db".to_string(), e.to_string()))?
            .expect("Image manifest is empty");

        let file_content = std::str::from_utf8(db_content)?;

        let image_config: Value =
            serde_json::from_str(file_content).expect("cannot parse image config from database");
        debug!(
            "Raw container image config {}",
            &image_config["config"]["Labels"].to_string()
        );

        Ok(serde_json::from_str::<ContainerImageMetadata>(
            &image_config["config"]["Labels"].to_string(),
        )?)
    }
}

fn get_image_content_key(image: &Reference) -> String {
    // Try to get the tag, if it doesn't exist, get the digest
    // if neither exist, return "latest" as the tag
    let tag = match image.tag() {
        Some(t) => t,
        _ => match image.digest() {
            Some(d) => d,
            _ => "latest",
        },
    };

    format!(
        "{}_{}_{}",
        image.registry(),
        image.repository().replace('/', "_"),
        tag
    )
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::*;

    #[tokio::test]
    async fn image_pull_and_bytecode_verify() {
        let database = sled::Config::new().temporary(true).open().unwrap();

        let (_tx, rx) = mpsc::channel(32);
        let mut mgr = ImageManager::new(database.clone(), true, rx).await.unwrap();

        let (image_content_key, _) = mgr
            .get_image(
                "quay.io/bpfman-bytecode/xdp_pass:latest",
                ImagePullPolicy::Always,
                None,
                None,
            )
            .await
            .expect("failed to pull bytecode");

        // Assert that an manifest, config and bytecode key were formed for image.
        assert!(database.scan_prefix(image_content_key.clone()).count() == 3);

        let program_bytes = mgr
            .get_bytecode_from_image_store(image_content_key)
            .await
            .expect("failed to get bytecode from image store");

        assert!(!program_bytes.is_empty())
    }

    #[tokio::test]
    async fn image_pull_policy_never_failure() {
        let database = sled::Config::new().temporary(true).open().unwrap();

        let (_tx, rx) = mpsc::channel(32);
        let mut mgr = ImageManager::new(database.clone(), true, rx).await.unwrap();

        let result = mgr
            .get_image(
                "quay.io/bpfman-bytecode/xdp_pass:latest",
                ImagePullPolicy::Never,
                None,
                None,
            )
            .await;

        assert_matches!(result, Err(ImageError::ByteCodeImageNotfound(_)));
    }

    #[tokio::test]
    #[should_panic]
    async fn private_image_pull_failure() {
        let database = sled::Config::new().temporary(true).open().unwrap();

        let (_tx, rx) = mpsc::channel(32);
        let mut mgr = ImageManager::new(database, true, rx).await.unwrap();

        mgr.get_image(
            "quay.io/bpfman-bytecode/xdp_pass_private:latest",
            ImagePullPolicy::Always,
            None,
            None,
        )
        .await
        .expect("failed to pull bytecode");
    }

    #[tokio::test]
    async fn private_image_pull_and_bytecode_verify() {
        env_logger::init();
        let database = sled::Config::new().temporary(true).open().unwrap();

        let (_tx, rx) = mpsc::channel(32);
        let mut mgr = ImageManager::new(database.clone(), true, rx).await.unwrap();

        let (image_content_key, _) = mgr
            .get_image(
                "quay.io/bpfman-bytecode/xdp_pass_private:latest",
                ImagePullPolicy::Always,
                Some("bpfman-bytecode+bpfmancreds".to_owned()),
                Some("D49CKWI1MMOFGRCAT8SHW5A56FSVP30TGYX54BBWKY2J129XRI6Q5TVH2ZZGTJ1M".to_owned()),
            )
            .await
            .expect("failed to pull bytecode");

        // Assert that an manifest, config and bytecode key were formed for image.
        assert!(database.scan_prefix(image_content_key.clone()).count() == 3);

        let program_bytes = mgr
            .get_bytecode_from_image_store(image_content_key)
            .await
            .expect("failed to get bytecode from image store");

        assert!(!program_bytes.is_empty())
    }

    #[tokio::test]
    async fn image_pull_failure() {
        let database = sled::Config::new().temporary(true).open().unwrap();

        let (_tx, rx) = mpsc::channel(32);
        let mut mgr = ImageManager::new(database, true, rx).await.unwrap();

        let result = mgr
            .get_image(
                "quay.io/bpfman-bytecode/xdp_pass:latest",
                ImagePullPolicy::Never,
                None,
                None,
            )
            .await;

        assert_matches!(result, Err(ImageError::ByteCodeImageNotfound(_)));
    }

    #[tokio::test]
    async fn test_good_image_content_key() {
        struct Case {
            input: &'static str,
            output: &'static str,
        }
        let tt = vec![
            Case{input: "busybox", output: "docker.io_library_busybox_latest"},
            Case{input:"quay.io/busybox", output: "quay.io_busybox_latest"},
            Case{input:"docker.io/test:tag", output: "docker.io_library_test_tag"},
            Case{input:"quay.io/test:5000", output: "quay.io_test_5000"},
            Case{input:"test.com/repo:tag", output: "test.com_repo_tag"},
            Case{
                input:"test.com/repo@sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                output: "test.com_repo_sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            }
        ];

        for t in tt {
            let good_reference: Reference = t.input.parse().unwrap();
            let image_content_key = get_image_content_key(&good_reference);
            assert_eq!(image_content_key, t.output);
        }
    }
}
