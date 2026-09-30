build:
    cargo build --bin ashpd-demo

run:
    cargo run --bin ashpd-demo

attach-kde:
    just --justfile ../kwin/Justfile attach

attach-gnome:
    just --justfile ../mutter/Justfile attach
