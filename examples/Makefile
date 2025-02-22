## Manage what image of each examples to run
##
## Default:
##   make deploy-xdp
##     quay.io/bpfman-bytecode/go-xdp-counter:latest
##     quay.io/bpfman-userspace/go-xdp-counter:latest
##
## Example: Override the upstream tag
##   make deploy-tc TAG=v0.1.0
##     quay.io/bpfman-bytecode/go-tc-counter:v0.1.0
##     quay.io/bpfman-userspace/go-uc-counter:v0.1.0
##
## Example: Run from private image
##   make deploy-tracepoint IMAGE_TP_US=docker.io/user1/tracepoint-userspace:test \
##                          IMAGE_TP_BC=docker.io/user1/tracepoint-bytecode:test
##     docker.io/user1/tracepoint-bytecode:test
##     docker.io/user1/tracepoint-userspace:test
##
## Example: Override the upstream tag on all images
##   make deploy TAG=v0.1.0
##     quay.io/bpfman-bytecode/go-tc-counter:v0.1.0
##     quay.io/bpfman-userspace/go-uc-counter:v0.1.0
##     quay.io/bpfman-bytecode/go-tracepoint-counter:v0.1.0
##     quay.io/bpfman-userspace/go-tracepoint-counter:v0.1.0
##     quay.io/bpfman-bytecode/go-xdp-counter:v0.1.0
##     quay.io/bpfman-userspace/go-xdp-counter:v0.1.0
##

.DEFAULT_GOAL := help

# Get the currently used golang install path (in GOPATH/bin, unless GOBIN is set)
ifeq (,$(shell go env GOBIN))
GOBIN=$(shell go env GOPATH)/bin
else
GOBIN=$(shell go env GOBIN)
endif

# Setting SHELL to bash allows bash commands to be executed by recipes.
# Options are set to exit when a recipe line exits non-zero or a piped command fails.
SHELL = /usr/bin/env bash -o pipefail
.SHELLFLAGS = -ec

.PHONY: all
all: build

##@ General

# The help target prints out all targets with their descriptions organized
# beneath their categories. The categories are represented by '##@' and the
# target descriptions by '##'. The awk commands is responsible for reading the
# entire set of makefiles included in this invocation, looking for lines of the
# file as xyz: ## something, and then pretty-format the target and help. Then,
# if there's a line with ##@ something, that gets pretty-printed as a category.
# More info on the usage of ANSI control characters for terminal formatting:
# https://en.wikipedia.org/wiki/ANSI_escape_code#SGR_parameters
# More info on the awk command:
# http://linuxcommand.org/lc3_adv_awk.php

.PHONY: help
help: ## Display this help.
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage:\n  make \033[36m<target>\033[0m\n  make \033[36mdeploy\033[0m TAG=v0.2.0\n  make \033[36mdeploy-xdp\033[0m IMAGE_XDP_US=quay.io/user1/go-xdp-counter-userspace:test\n"} /^[a-zA-Z_0-9-]+:.*?##/ { printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2 } /^##@/ { printf "\n\033[1m%s\033[0m\n", substr($$0, 5) } ' $(MAKEFILE_LIST)

##@ Local Dependencies
ignore-not-found ?= false

## Location to install dependencies to
LOCALBIN ?= $(shell pwd)/bin
$(LOCALBIN):
	mkdir -p $(LOCALBIN)

## Tool Binaries
KUSTOMIZE ?= $(LOCALBIN)/kustomize

## Tool Versions
KUSTOMIZE_VERSION ?= v3.8.7

KUSTOMIZE_INSTALL_SCRIPT ?= "https://raw.githubusercontent.com/kubernetes-sigs/kustomize/master/hack/install_kustomize.sh"
.PHONY: kustomize
kustomize: $(KUSTOMIZE) ## Download kustomize locally if necessary.
$(KUSTOMIZE): $(LOCALBIN)
	test -s $(LOCALBIN)/kustomize || { curl -Ss $(KUSTOMIZE_INSTALL_SCRIPT) | bash -s -- $(subst v,,$(KUSTOMIZE_VERSION)) $(LOCALBIN); }

##@ Development

.PHONY: fmt
fmt: ## Run go fmt against code.
	go fmt ./...

.PHONY: verify
verify: ## Verify all the autogenerated code
	./hack/verify-codegen.sh

##@ Build

.PHONY: build
build: fmt ## Build all the userspace example code.
	go build -o go-tc-counter/go-tc-counter go-tc-counter/main.go
	go build -o go-tracepoint-counter/go-tracepoint-counter go-tracepoint-counter/main.go
	go build -o go-xdp-counter/go-xdp-counter go-xdp-counter/main.go

.PHONY: generate
generate: ## Run `go generate` to build the bytecode for each of the examples.
	go generate ./...

.PHONY: build-us-images
build-us-images: ## Build all example userspace images
	docker build -t ${IMAGE_TC_US} -f ./go-tc-counter/container-deployment/Containerfile.go-tc-counter ../
	docker build -t ${IMAGE_TP_US} -f ./go-tracepoint-counter/container-deployment/Containerfile.go-tracepoint-counter ../
	docker build -t ${IMAGE_XDP_US} -f ./go-xdp-counter/container-deployment/Containerfile.go-xdp-counter ../

.PHONY: build-bc-images
build-bc-images: generate ## Build bytecode example userspace images
	IMAGE_TC_BC=${IMAGE_TC_BC} IMAGE_TP_BC=${IMAGE_TP_BC} IMAGE_XDP_BC=${IMAGE_XDP_BC} ./build-bytecode-images.sh

.PHONY: push-us-images
push-us-images: ## Push all example userspace images
	docker push ${IMAGE_TC_US}
	docker push ${IMAGE_TP_US}
	docker push ${IMAGE_XDP_US}

.PHONY: push-bc-images
push-bc-images: ## Push all example userspace images
	docker push ${IMAGE_TC_BC}
	docker push ${IMAGE_TP_BC}
	docker push ${IMAGE_XDP_BC}


.PHONY: load-us-images-kind
load-us-images-kind: build-us-images ## Build and load all example userspace images into kind
	kind load docker-image ${IMAGE_TC_US} ${IMAGE_TP_US} ${IMAGE_XDP_US} --name ${KIND_CLUSTER_NAME}

##@ Deployment Variables (not commands)
TAG: ## Used to set all images to a fixed tag. Example: make deploy TAG=v0.2.0
IMAGE_TC_BC: ## TC Bytecode image. Example: make deploy-tc IMAGE_TC_BC=quay.io/user1/go-tc-counter-bytecode:test
IMAGE_TC_US: ## TC Userspace image. Example: make deploy-tc IMAGE_TC_US=quay.io/user1/go-tc-counter-userspace:test
IMAGE_TP_BC: ## Tracepoint Bytecode image. Example: make deploy-tracepoint IMAGE_TP_BC=quay.io/user1/go-tracepoint-counter-bytecode:test
IMAGE_TP_US: ## Tracepoint Userspace image. Example: make deploy-tracepoint IMAGE_TP_US=quay.io/user1/go-tracepoint-counter-userspace:test
IMAGE_XDP_BC: ## XDP Bytecode image. Example: make deploy-xdp IMAGE_XDP_BC=quay.io/user1/go-xdp-counter-bytecode:test
IMAGE_XDP_US: ## XDP Userspace image. Example: make deploy-xdp IMAGE_XDP_US=quay.io/user1/go-xdp-counter-userspace:test
KIND_CLUSTER_NAME: ## Name of the deployed cluster to load example images to, defaults to `bpfman-deployment`
ignore-not-found: ## For any undeploy command, set to true to ignore resource not found errors during deletion. Example: make undeploy ignore-not-found=true

##@ Deployment
IMAGE_TC_BC ?= quay.io/bpfman-bytecode/go-tc-counter:latest
IMAGE_TC_US ?= quay.io/bpfman-userspace/go-tc-counter:latest
IMAGE_TP_BC ?= quay.io/bpfman-bytecode/go-tracepoint-counter:latest
IMAGE_TP_US ?= quay.io/bpfman-userspace/go-tracepoint-counter:latest
IMAGE_XDP_BC ?= quay.io/bpfman-bytecode/go-xdp-counter:latest
IMAGE_XDP_US ?= quay.io/bpfman-userspace/go-xdp-counter:latest
KUST_DIR=default
KIND_CLUSTER_NAME ?= bpfman-deployment

.PHONY: deploy-tc
deploy-tc: kustomize ## Deploy go-tc-counter to the cluster specified in ~/.kube/config.
ifndef TAG
	sed 's@URL_BC@$(IMAGE_TC_BC)@' config/default/go-tc-counter/patch.yaml.env > config/default/go-tc-counter/patch.yaml
	cd config/default/go-tc-counter && $(KUSTOMIZE) edit set image quay.io/bpfman-userspace/go-tc-counter=${IMAGE_TC_US}
else
	$(eval KUST_DIR=$(TAG))
endif
	$(KUSTOMIZE) build config/$(KUST_DIR)/go-tc-counter | kubectl apply -f -

.PHONY: undeploy-tc
undeploy-tc: ## Undeploy go-tc-counter from the cluster specified in ~/.kube/config.
ifndef TAG
	sed 's@URL_BC@$(IMAGE_TC_BC)@' config/default/go-tc-counter/patch.yaml.env > config/default/go-tc-counter/patch.yaml
	cd config/default/go-tc-counter && $(KUSTOMIZE) edit set image quay.io/bpfman-userspace/go-tc-counter=${IMAGE_TC_US}
else
	$(eval KUST_DIR=$(TAG))
endif
	$(KUSTOMIZE) build config/$(KUST_DIR)/go-tc-counter | kubectl delete --ignore-not-found=$(ignore-not-found) -f -


.PHONY: deploy-tracepoint
deploy-tracepoint: kustomize ## Deploy go-tracepoint-counter to the cluster specified in ~/.kube/config.
ifndef TAG
	sed 's@URL_BC@$(IMAGE_TP_BC)@' config/default/go-tracepoint-counter/patch.yaml.env > config/default/go-tracepoint-counter/patch.yaml
	cd config/default/go-tracepoint-counter && $(KUSTOMIZE) edit set image quay.io/bpfman-userspace/go-tracepoint-counter=${IMAGE_TP_US}
else
	$(eval KUST_DIR=$(TAG))
endif
	$(KUSTOMIZE) build config/$(KUST_DIR)/go-tracepoint-counter | kubectl apply -f -

.PHONY: undeploy-tracepoint
undeploy-tracepoint: ## Undeploy go-tracepoint-counter from the cluster specified in ~/.kube/config.
ifndef TAG
	sed 's@URL_BC@$(IMAGE_TP_BC)@' config/default/go-tracepoint-counter/patch.yaml.env > config/default/go-tracepoint-counter/patch.yaml
	cd config/default/go-tracepoint-counter && $(KUSTOMIZE) edit set image quay.io/bpfman-userspace/go-tracepoint-counter=${IMAGE_TP_US}
else
	$(eval KUST_DIR=$(TAG))
endif
	$(KUSTOMIZE) build config/$(KUST_DIR)/go-tracepoint-counter | kubectl delete --ignore-not-found=$(ignore-not-found) -f -


.PHONY: deploy-xdp
deploy-xdp: kustomize ## Deploy go-xdp-counter to the cluster specified in ~/.kube/config.
ifndef TAG
	sed 's@URL_BC@$(IMAGE_XDP_BC)@' config/default/go-xdp-counter/patch.yaml.env > config/default/go-xdp-counter/patch.yaml
	cd config/default/go-xdp-counter && $(KUSTOMIZE) edit set image quay.io/bpfman-userspace/go-xdp-counter=${IMAGE_XDP_US}
else
	$(eval KUST_DIR=$(TAG))
endif
	$(KUSTOMIZE) build config/$(KUST_DIR)/go-xdp-counter | kubectl apply -f -

.PHONY: undeploy-xdp
undeploy-xdp: ## Undeploy go-xdp-counter from the cluster specified in ~/.kube/config.
ifndef TAG
	sed 's@URL_BC@$(IMAGE_XDP_BC)@' config/default/go-xdp-counter/patch.yaml.env > config/default/go-xdp-counter/patch.yaml
	cd config/default/go-xdp-counter && $(KUSTOMIZE) edit set image quay.io/bpfman-userspace/go-xdp-counter=${IMAGE_XDP_US}
else
	$(eval KUST_DIR=$(TAG))
endif
	$(KUSTOMIZE) build config/$(KUST_DIR)/go-xdp-counter | kubectl delete --ignore-not-found=$(ignore-not-found) -f -


.PHONY: deploy-xdp-ms
deploy-xdp-ms: kustomize ## Deploy go-xdp-counter-sharing-map (shares map with go-xdp-counter) to the cluster specified in ~/.kube/config.
ifndef TAG
	sed 's@URL_BC@$(IMAGE_XDP_BC)@' config/default/go-xdp-counter-sharing-map/patch.yaml.env > config/default/go-xdp-counter-sharing-map/patch.yaml
	cd config/default/go-xdp-counter-sharing-map && $(KUSTOMIZE) edit set image quay.io/bpfman-userspace/go-xdp-counter=${IMAGE_XDP_US}
else
	$(eval KUST_DIR=$(TAG))
endif
	$(KUSTOMIZE) build config/$(KUST_DIR)/go-xdp-counter-sharing-map | kubectl apply -f -

.PHONY: undeploy-xdp-ms
undeploy-xdp-ms: ## Undeploy go-xdp-counter-sharing-map from the cluster specified in ~/.kube/config.
ifndef TAG
	sed 's@URL_BC@$(IMAGE_XDP_BC)@' config/default/go-xdp-counter-sharing-map/patch.yaml.env > config/default/go-xdp-counter-sharing-map/patch.yaml
	cd config/default/go-xdp-counter-sharing-map && $(KUSTOMIZE) edit set image quay.io/bpfman-userspace/go-xdp-counter=${IMAGE_XDP_US}
else
	$(eval KUST_DIR=$(TAG))
endif
	$(KUSTOMIZE) build config/$(KUST_DIR)/go-xdp-counter-sharing-map | kubectl delete --ignore-not-found=$(ignore-not-found) -f -


.PHONY: deploy
deploy: deploy-tc deploy-tracepoint deploy-xdp deploy-xdp-ms  ## Deploy all examples to the cluster specified in ~/.kube/config.

.PHONY: undeploy
undeploy: undeploy-tc undeploy-tracepoint undeploy-xdp undeploy-xdp-ms  ## Undeploy all examples to the cluster specified in ~/.kube/config.
