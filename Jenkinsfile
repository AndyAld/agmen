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
    // The rust container's docker CLI, for the it-restart tests.
    DOCKER_CLI_VERSION = '29.9.2'
    COMPOSE_VERSION   = 'v5.6.1'
    CLAUDE_CODE_VERSION = '2.1.310'
    // agmen's own registry; a placeholder until it exists. The Jenkins
    // credential agmen-registry (username and password) logs in to it.
    REGISTRY          = 'registry.agmen.example'
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
            curl -fsSL "https://download.docker.com/linux/static/stable/${arch}/docker-${DOCKER_CLI_VERSION}.tgz" \
              | tar -xz -C /usr/local/bin --strip-components=1 docker/docker
            mkdir -p /usr/local/lib/docker/cli-plugins
            plugin=/usr/local/lib/docker/cli-plugins/docker-compose
            base="https://github.com/docker/compose/releases/download/${COMPOSE_VERSION}/docker-compose-linux-${arch}"
            curl -fsSL -o "${plugin}" "${base}"
            echo "$(curl -fsSL "${base}.sha256" | cut -d' ' -f1)  ${plugin}" | sha256sum -c -
            chmod +x "${plugin}"
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

    stage('Image') {
      when {
        branch 'main'
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
              image="${REGISTRY}/agmen-hub:build-${BUILD_NUMBER}"
              docker build -f crates/agmen-hub/Dockerfile -t "${image}" .
              printf '%s' "${REGISTRY_PASSWORD}" | docker login -u "${REGISTRY_USER}" --password-stdin "${REGISTRY}"
              docker push "${image}"
              docker logout "${REGISTRY}"
            '''
          }
        }
      }
    }
  }
}
