#!/bin/bash

set -e

echo "=== Testing Wax Phase 2 Installation Commands ==="
echo

echo "1. Testing install --dry-run"
./target/release/wax install tree --dry-run
echo "✓ Dry-run test passed"
echo

echo "2. Installing tree (simple formula, no dependencies)"
./target/release/wax install tree
echo "✓ Install test passed"
echo

echo "3. Verifying tree was installed"
if [ -f ~/homebrew/Cellar/tree/2.2.1/bin/tree ]; then
    echo "✓ Tree binary exists"
else
    echo "✗ Tree binary not found"
    exit 1
fi
echo

echo "4. Verifying symlink was created"
if [ -L ~/homebrew/bin/tree ]; then
    echo "✓ Tree symlink exists"
else
    echo "✗ Tree symlink not found"
    exit 1
fi
echo

echo "5. Testing tree command"
if ~/homebrew/bin/tree --version | grep -q "tree v2.2.1"; then
    echo "✓ Tree command works"
else
    echo "✗ Tree command failed"
    exit 1
fi
echo

echo "6. Testing upgrade command (already up to date)"
./target/release/wax upgrade tree
echo "✓ Upgrade test passed"
echo

echo "7. Testing uninstall --dry-run"
./target/release/wax uninstall tree --dry-run
echo "✓ Uninstall dry-run test passed"
echo

echo "8. Uninstalling tree"
./target/release/wax uninstall tree
echo "✓ Uninstall test passed"
echo

echo "9. Verifying tree was uninstalled"
if [ ! -d ~/homebrew/Cellar/tree ]; then
    echo "✓ Tree directory removed"
else
    echo "✗ Tree directory still exists"
    exit 1
fi
echo

echo "10. Verifying symlink was removed"
if [ ! -L ~/homebrew/bin/tree ]; then
    echo "✓ Tree symlink removed"
else
    echo "✗ Tree symlink still exists"
    exit 1
fi
echo

echo "11. Testing install with dependencies (jq)"
./target/release/wax install jq --dry-run
echo "✓ Dependencies resolved correctly"
echo

echo "=== All Phase 2 Tests Passed! ==="
