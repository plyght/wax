# Wax Examples

Tiny, runnable scenarios that show what the `wax` CLI does. Each one is a shell
script that calls the real binary when it is available, and prints the expected
commands when it is not.

## Quick start

```bash
# Build a binary once (or install wax), then run everything:
cargo build
./run_examples.sh
```

You can also point the examples at a specific binary:

```bash
WAX_BIN=/usr/local/bin/wax ./run_examples.sh
```

## Examples

### 1. [search](search/)

Search formulae and casks from the cached Homebrew index.

```bash
cd search
./run.sh
```

**Demonstrates:**
- Querying the local formula/cask index
- Formula vs cask auto-detection

### 2. [info](info/)

Show details for a single formula.

```bash
cd info
./run.sh
```

**Demonstrates:**
- Reading formula metadata (version, deps, bottle info)
- The `wax info` / `wax show` command

## Directory structure

```
examples/
├── README.md
├── run_examples.sh
├── search/
│   ├── README.md
│   └── run.sh
└── info/
    ├── README.md
    └── run.sh
```
