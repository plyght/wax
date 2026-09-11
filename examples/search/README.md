# Search Example

Search formulae and casks by name.

## Run

```bash
./run.sh
```

The script uses `WAX_BIN`, then `wax` on `PATH`, then `target/debug/wax`, then
`target/release/wax`. If none exist it prints the expected commands instead.

## What it demonstrates

1. **Index search** — `wax search <query>` reads the local formula/cask cache.
2. **Auto-detection** — hits include both formulae and casks, no `--cask` flag needed.

## Expected commands

```bash
wax update        # fetch/refresh the index once
wax search nginx
```
