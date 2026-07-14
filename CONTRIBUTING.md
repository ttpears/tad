# Contributing

tad is a Rust project; `tmux` is the only runtime dependency.

## Building and testing

```sh
make build          # cargo build --release
make test           # cargo test
make install        # install binary + completions under ~/.local
make uninstall
```

`make install` honors `PREFIX` (default `$(HOME)/.local`) if you want
the binary and completions somewhere else.

## Maintainer docs

- [docs/RELEASING.md](docs/RELEASING.md) — cutting a release (GitHub,
  AUR)
- [docs/SCREENSHOTS.md](docs/SCREENSHOTS.md) — regenerating the README
  demo gif and theme stills
