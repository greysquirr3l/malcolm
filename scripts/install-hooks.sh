#!/usr/bin/env bash
# Install git hooks (and the gitleaks pre-commit hook) for the malcolm workspace.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
GIT_DIR="$(git rev-parse --git-dir)"

echo "Installing git hooks into $GIT_DIR/hooks..."

# Install our pre-commit, commit-msg, and pre-push hooks
cp "$SCRIPT_DIR/pre-commit" "$GIT_DIR/hooks/pre-commit"
chmod +x "$GIT_DIR/hooks/pre-commit"
echo "  ✓ pre-commit"

cp "$SCRIPT_DIR/commit-msg" "$GIT_DIR/hooks/commit-msg"
chmod +x "$GIT_DIR/hooks/commit-msg"
echo "  ✓ commit-msg"

cp "$SCRIPT_DIR/pre-push" "$GIT_DIR/hooks/pre-push"
chmod +x "$GIT_DIR/hooks/pre-push"
echo "  ✓ pre-push"

# Install the standalone gitleaks pre-commit hook IF gitleaks is available
# and the operator opts in. The bundled pre-commit above already runs
# `gitleaks protect --staged --verbose` inline, so this is opt-in for
# projects that prefer the official hook.
echo ""
if command -v gitleaks &> /dev/null; then
    if [ "${MALCOLM_INSTALL_GITLEAKS_HOOK:-0}" = "1" ]; then
        # gitleaks 8 ships a pre-commit hook via `gitleaks hook`; older
        # versions used `gitleaks install`. Try the modern form first.
        if gitleaks hook --help &> /dev/null 2>&1; then
            (cd "$REPO_ROOT" && gitleaks hook --target=pre-commit --install)
            echo "  ✓ gitleaks pre-commit hook (standalone)"
        else
            # Fall back: chain the official hook into our pre-commit so
            # the operator still gets a standalone invocation.
            GITLEAKS_HOOK="$GIT_DIR/hooks/_gitleaks_pre_commit"
            cat > "$GITLEAKS_HOOK" <<'EOF'
#!/usr/bin/env bash
exec gitleaks protect --staged --verbose
EOF
            chmod +x "$GITLEAKS_HOOK"
            echo "  ✓ gitleaks pre-commit hook (chain shim at $GITLEAKS_HOOK)"
        fi
    else
        echo "→ Skipping standalone gitleaks pre-commit hook (set MALCOLM_INSTALL_GITLEAKS_HOOK=1 to install)."
    fi
else
    echo "⚠️  gitleaks not installed; install with: brew install gitleaks"
fi

echo ""
echo "Git hooks installed successfully!"
echo ""
echo "pre-commit (runs on every commit):"
echo "  • Format code with cargo fmt"
echo "  • Detect secrets with gitleaks"
echo "  • Check vulnerabilities with cargo audit"
echo ""
echo "commit-msg (validates commit messages):"
echo "  • Enforce conventional commits format (feat:, fix:, etc.)"
echo "  • Warn when subject line is over 72 characters"
echo "  • Ensure proper formatting (blank line between subject/body)"
echo ""
echo "pre-push (runs before push to remote):"
echo "  • Run all workspace tests"
echo "  • Run clippy with -D warnings across all targets and features"
echo "  • Verify release build"
echo "  • Build documentation with RUSTDOCFLAGS='-D warnings'"
echo ""
echo "To bypass hooks (not recommended):"
echo "  git commit --no-verify"
echo "  git push --no-verify"
