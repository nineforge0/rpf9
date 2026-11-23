# rpf9

Rust reader for RPF9 (`9FPR` / `0x52504639`) archives used in Large Steal Car 6.

## Examples

```sh
cargo run --example cli -- path/to/common.rpf
cargo run --example cli -- path/to/common.rpf --list --limit 40
cargo run --example cli -- path/to/common.rpf --list --select 1-10 --kind zlib
cargo run --example cli -- path/to/common.rpf --extract out --select 1-10
cargo run --example cli -- path/to/common.rpf --dump @0x81430 > entry.bin
cargo run --example cli -- path/to/common.rpf --hexdump 0 --hex-limit 256
cargo run --example cli -- path/to/common.rpf --text 0
cargo run --example cli -- path/to/common.rpf --json --select 0-2
cargo run --example cli -- path/to/common.rpf --copy copy.rpf
cargo run --example tui -- path/to/common.rpf
```
