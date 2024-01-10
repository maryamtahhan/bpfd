/*
Copyright 2022.

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
*/

package main

import (
	"context"
	"flag"
	"fmt"
	"os"

	"go.uber.org/zap/zapcore"
	_ "k8s.io/client-go/plugin/pkg/client/auth"

	"google.golang.org/grpc/credentials/insecure"
	"k8s.io/apimachinery/pkg/runtime"
	utilruntime "k8s.io/apimachinery/pkg/util/runtime"
	clientgoscheme "k8s.io/client-go/kubernetes/scheme"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/healthz"
	"sigs.k8s.io/controller-runtime/pkg/log/zap"

	bpfdiov1alpha1 "github.com/bpfd-dev/bpfd/bpfd-operator/apis/v1alpha1"
	bpfdagent "github.com/bpfd-dev/bpfd/bpfd-operator/controllers/bpfd-agent"

	"github.com/bpfd-dev/bpfd/bpfd-operator/internal/conn"
	gobpfd "github.com/bpfd-dev/bpfd/clients/gobpfd/v1"
	v1 "k8s.io/api/core/v1"
	//+kubebuilder:scaffold:imports
)

var (
	scheme   = runtime.NewScheme()
	setupLog = ctrl.Log.WithName("setup")
)

func init() {
	utilruntime.Must(clientgoscheme.AddToScheme(scheme))
	utilruntime.Must(bpfdiov1alpha1.AddToScheme(scheme))
	utilruntime.Must(v1.AddToScheme(scheme))
	//+kubebuilder:scaffold:scheme
}

func main() {
	var metricsAddr string
	var probeAddr string
	var opts zap.Options
	flag.StringVar(&metricsAddr, "metrics-bind-address", ":8080", "The address the metric endpoint binds to.")
	flag.StringVar(&probeAddr, "health-probe-bind-address", ":8081", "The address the probe endpoint binds to.")
	flag.Parse()

	// Get the Log level for bpfd deployment where this pod is running
	logLevel := os.Getenv("GO_LOG")
	switch logLevel {
	case "info":
		opts = zap.Options{
			Development: false,
		}
	case "debug":
		opts = zap.Options{
			Development: true,
		}
	case "trace":
		opts = zap.Options{
			Development: true,
			Level:       zapcore.Level(-2),
		}
	default:
		opts = zap.Options{
			Development: false,
		}
	}

	ctrl.SetLogger(zap.New(zap.UseFlagOptions(&opts)))

	setupLog.Info("ABOUT TO CALL NEW MANAGER");

	mgr, err := ctrl.NewManager(ctrl.GetConfigOrDie(), ctrl.Options{
		Scheme: scheme,
		MetricsBindAddress:     ":8090",
		Port: 9443,
		HealthProbeBindAddress: ":8091",
		LeaderElection: false,
		// Specify that Secrets's should not be cached.
		ClientDisableCacheFor: []client.Object{&v1.Secret{}},
	})
	if err != nil {
		setupLog.Error(err, "unable to start manager")
		os.Exit(1)
	}

	// Setup bpfd Client
	configFileData := conn.LoadConfig()
	setupLog.Info("Connecting over UNIX socket to bpfd")

	// Set up a connection to bpfd, block until bpfd is up.
	setupLog.Info("Waiting for active connection to bpfd", "endpoints", configFileData.Grpc.Endpoints)
	conn, err := conn.CreateConnection(configFileData.Grpc.Endpoints, context.Background(), insecure.NewCredentials())
	if err != nil {
		setupLog.Error(err, "unable to connect to bpfd")
		os.Exit(1)
	}

	nodeName := os.Getenv("KUBE_NODE_NAME")
	if nodeName == "" {
		setupLog.Error(fmt.Errorf("KUBE_NODE_NAME env var not set"), "Couldn't determine bpfd-agent's node")
		os.Exit(1)
	}

	common := bpfdagent.ReconcilerCommon{
		Client:     mgr.GetClient(),
		Scheme:     mgr.GetScheme(),
		GrpcConn:   conn,
		BpfdClient: gobpfd.NewBpfdClient(conn),
		NodeName:   nodeName,
	}

	if err = (&bpfdagent.XdpProgramReconciler{
		ReconcilerCommon: common,
	}).SetupWithManager(mgr); err != nil {
		setupLog.Error(err, "unable to create xdpProgram controller", "controller", "BpfProgram")
		os.Exit(1)
	}

	if err = (&bpfdagent.TcProgramReconciler{
		ReconcilerCommon: common,
	}).SetupWithManager(mgr); err != nil {
		setupLog.Error(err, "unable to create tcProgram controller", "controller", "BpfProgram")
		os.Exit(1)
	}

	if err = (&bpfdagent.TracepointProgramReconciler{
		ReconcilerCommon: common,
	}).SetupWithManager(mgr); err != nil {
		setupLog.Error(err, "unable to create tracepointProgram controller", "controller", "BpfProgram")
		os.Exit(1)
	}

	if err = (&bpfdagent.KprobeProgramReconciler{
		ReconcilerCommon: common,
	}).SetupWithManager(mgr); err != nil {
		setupLog.Error(err, "unable to create kprobeProgram controller", "controller", "BpfProgram")
		os.Exit(1)
	}

	if err = (&bpfdagent.UprobeProgramReconciler{
		ReconcilerCommon: common,
	}).SetupWithManager(mgr); err != nil {
		setupLog.Error(err, "unable to create uprobeProgram controller", "controller", "BpfProgram")
		os.Exit(1)
	}

	if err = (&bpfdagent.DiscoveredProgramReconciler{
		ReconcilerCommon: common,
	}).SetupWithManager(mgr); err != nil {
		setupLog.Error(err, "unable to create discoveredProgram controller", "controller", "BpfProgram")
		os.Exit(1)
	}

	//+kubebuilder:scaffold:builder

	if err := mgr.AddHealthzCheck("healthz", healthz.Ping); err != nil {
		setupLog.Error(err, "unable to set up health check")
		os.Exit(1)
	}
	if err := mgr.AddReadyzCheck("readyz", healthz.Ping); err != nil {
		setupLog.Error(err, "unable to set up ready check")
		os.Exit(1)
	}

	setupLog.Info("starting Bpfd-Agent")
	if err := mgr.Start(ctrl.SetupSignalHandler()); err != nil {
		setupLog.Error(err, "problem running manager")
		os.Exit(1)
	}
}
