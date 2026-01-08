#!/bin/bash
set -e

echo "======================================"
echo "Wax Integration Test Suite"
echo "======================================"
echo

WAX=./target/debug/wax

echo "[TEST 1] Testing tap commands"
echo "------------------------------"
echo "$ wax tap list"
$WAX tap list
echo

echo "$ wax tap add homebrew/services"
$WAX tap add homebrew/services || echo "Note: homebrew/services has no Formula directory"
echo

echo "$ wax tap list"
$WAX tap list
echo

echo "$ wax tap remove homebrew/services"
$WAX tap remove homebrew/services 2>/dev/null || true
echo

echo "[TEST 2] Testing search with tap formulae"
echo "------------------------------------------"
echo "$ wax search nginx | head -20"
$WAX search nginx | head -20
echo

echo "[TEST 3] Testing info command"
echo "------------------------------"
echo "$ wax info tree"
$WAX info tree
echo

echo "[TEST 4] Testing bottle installation (fast path)"
echo "------------------------------------------------"
echo "$ wax install hello --user --dry-run"
$WAX install hello --user --dry-run
echo

echo "[TEST 5] Checking if --build-from-source flag is recognized"
echo "------------------------------------------------------------"
echo "$ wax install hello --build-from-source --user --dry-run"
$WAX install hello --build-from-source --user --dry-run
echo

echo "======================================"
echo "Integration tests completed!"
echo "======================================"
