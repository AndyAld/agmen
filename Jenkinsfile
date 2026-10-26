pipeline {
  agent {
    kubernetes {
      yamlFile 'ci/kubePod.yaml'
      retries 2
    }
  }

  environment {
    NEXTEST_VERSION   = '0.9.149'
    CARGO_DENY_VERSION = '0.20.2'
    COMPOSE           = 'docker compose -f deploy/dev/compose.yaml'
    // The rust container's docker CLI and compose plugin, for the
    // it-restart tests, pinned by sha256 per architecture. Docker publishes
    // no checksum for its static tarballs; these sums were taken on
    // 2026-10-26, and the docker binary in each tarball matched the one in
    // the docker:29.9.2-cli image for the same architecture. To bump, do the
    // same check again.
    DOCKER_CLI_VERSION = '29.9.2'
    DOCKER_CLI_SHA256_X86_64  = '9ac225999fd5e863f7694c728ef23d480616b1e5639328fa309befd4243d69eb'
    DOCKER_CLI_SHA256_AARCH64 = '873b3e0207857c3b2f28392fb922f3ea6efe37620c16d4d4aaf2fef409c90b73'
    COMPOSE_VERSION   = 'v5.6.1'
    COMPOSE_SHA256_X86_64  = '47737f4236e6d3ccea7291995203f8f018058783b4c3a9469119811a0b5db8c0'
    COMPOSE_SHA256_AARCH64 = '42cfa75f51ad0ccf290bad37899e0038a8c10a0069e85233ecc1d53e3ff1347d'
    CLAUDE_CODE_VERSION = '2.1.310'
    // agmen's image registry is AGMEN_REGISTRY, a global environment
    // variable set in Jenkins (Manage Jenkins, System). The Image stage is
    // skipped while it is unset (deploy/k8s/agmen-env/README.md).
  }

  stages {
    stage('Tools') {
      steps {
        container('rust') {
          sh '''
            set -eu
            cargo install --locked cargo-nextest --version "${NEXTEST_VERSION}"
            cargo install --locked cargo-deny --version "${CARGO_DENY_VERSION}"
          '''
        }
      }
    }

    stage('Gate') {
      steps {
        container('rust') {
          sh '''
            set -eu
            cargo fmt --all --check
            cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
            cargo nextest run --locked --workspace
            cargo test --locked --doc --workspace
            RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
            cargo deny --locked check
          '''
        }
      }
    }

    stage('Integration') {
      steps {
        container('docker') {
          sh '''
            set -eu
            apk add --no-cache bash
            deploy/dev/init.sh
            ${COMPOSE} up -d --wait
          '''
        }
        container('rust') {
          sh '''
            set -eu
            set -a; . deploy/dev/.secrets/dev.env; set +a
            cargo nextest run --locked --workspace --profile it --no-tests=warn
          '''
        }
        // Tests that restart EMQX run alone, after the rest, through the
        // dockerd socket shared with the docker container.
        container('rust') {
          sh '''
            set -eu
            arch="$(uname -m)"
            case "${arch}" in
              x86_64)
                docker_sha="${DOCKER_CLI_SHA256_X86_64}"
                compose_sha="${COMPOSE_SHA256_X86_64}"
                ;;
              aarch64)
                docker_sha="${DOCKER_CLI_SHA256_AARCH64}"
                compose_sha="${COMPOSE_SHA256_AARCH64}"
                ;;
              *)
                echo "no pinned docker CLI checksum for ${arch}" >&2
                exit 1
                ;;
            esac
            dl="$(mktemp -d)"
            curl -fsSL -o "${dl}/docker.tgz" \
              "https://download.docker.com/linux/static/stable/${arch}/docker-${DOCKER_CLI_VERSION}.tgz"
            echo "${docker_sha}  ${dl}/docker.tgz" | sha256sum -c -
            tar -xzf "${dl}/docker.tgz" -C /usr/local/bin --strip-components=1 docker/docker
            curl -fsSL -o "${dl}/docker-compose" \
              "https://github.com/docker/compose/releases/download/${COMPOSE_VERSION}/docker-compose-linux-${arch}"
            echo "${compose_sha}  ${dl}/docker-compose" | sha256sum -c -
            install -D -m 0755 "${dl}/docker-compose" /usr/local/lib/docker/cli-plugins/docker-compose
            rm -rf "${dl}"
            export DOCKER_HOST=unix:///var/run/dind/docker.sock
            set -a; . deploy/dev/.secrets/dev.env; set +a
            cargo nextest run --locked --workspace --profile it-restart --no-tests=warn
          '''
        }
      }
      post {
        always {
          container('docker') {
            sh '${COMPOSE} logs --no-color > compose.log 2>&1 || true'
            sh '${COMPOSE} down -v || true'
          }
          archiveArtifacts artifacts: 'compose.log', allowEmptyArchive: true
        }
      }
    }

    // Plugin manifest and tests on every branch, in the node container.
    // tsc stays out until the plugin types can be written without a person
    // (docs/adr/0006-claude-code-plugin.md).
    stage('Plugin') {
      steps {
        container('node') {
          sh '''
            set -eu
            npm install -g --no-fund --no-audit "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"
            if [ -f .claude-plugin/marketplace.json ]; then
              claude plugin validate .
            fi
            cd mods/agmen-claude
            claude plugin validate .
            claude plugin test .
          '''
        }
      }
    }

    // Builds and pushes the hub image from main once AGMEN_REGISTRY is set.
    // The Jenkins credential agmen-registry (username and password) logs in
    // to it.
    stage('Image') {
      when {
        allOf {
          branch 'main'
          expression { (env.AGMEN_REGISTRY ?: '').trim() != '' }
        }
      }
      steps {
        container('docker') {
          withCredentials([usernamePassword(
            credentialsId: 'agmen-registry',
            usernameVariable: 'REGISTRY_USER',
            passwordVariable: 'REGISTRY_PASSWORD'
          )]) {
            sh '''
              set -eu
              image="${AGMEN_REGISTRY}/agmen-hub:build-${BUILD_NUMBER}"
              docker build -f crates/agmen-hub/Dockerfile -t "${image}" .
              printf '%s' "${REGISTRY_PASSWORD}" | docker login -u "${REGISTRY_USER}" --password-stdin "${AGMEN_REGISTRY}"
              docker push "${image}"
              docker logout "${AGMEN_REGISTRY}"
            '''
          }
        }
      }
    }
  }
}
