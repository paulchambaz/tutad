run *ARGS:
  cargo run {{ ARGS }}

watch *ARGS:
  cargo watch -x run {{ ARGS }}

build:
  cargo build --release

test:
  cargo test --all-targets -- --include-ignored

watch-test:
  cargo watch -x test

coverage:
  cargo tarpaulin

sdk:
  scripts/sdk.sh

fmt:
  cargo fmt

lint:
  cargo clippy --all-targets -- -D warnings

man:
  scdoc < tutad.1.scd > tutad.1

install prefix=(env_var('HOME') + '/.local'): build man
  install -Dm755 target/release/tutad {{ prefix }}/bin/tutad
  install -Dm644 tutad.1 {{ prefix }}/share/man/man1/tutad.1
