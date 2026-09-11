# Info Example

Show metadata for a single formula.

## Run

```bash
./run.sh
```

The script uses `WAX_BIN`, then `wax` on `PATH`, then `target/debug/wax`, then
`target/release/wax`. If none exist it prints the expected commands instead.

## What it demonstrates

1. **Formula details** — `wax info <name>` prints version, dependencies, and bottle info.
2. **Alias** — `wax show nginx` behaves the same as `wax info nginx`.

## Expected commands

```bash
wax update       # fetch/refresh the index once
wax info nginx
wax show nginx   # alias
```
