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
  }
}
