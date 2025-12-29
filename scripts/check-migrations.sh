#!/bin/bash
# Check that existing migration files haven't been modified.
# Migrations are immutable - we can only add new ones.

set -e

MIGRATIONS_DIR="migrations"

# Get the base commit to compare against
if [ -n "$GITHUB_BASE_REF" ]; then
    # In a PR, compare against the base branch
    BASE="origin/$GITHUB_BASE_REF"
elif [ -n "$CI" ]; then
    # In CI on main branch, compare against previous commit
    BASE="HEAD~1"
else
    # Local development - compare against HEAD (staged changes)
    BASE="HEAD"
fi

# Get list of migration files that existed in the base commit
EXISTING_MIGRATIONS=$(git ls-tree --name-only "$BASE" -- "$MIGRATIONS_DIR" 2>/dev/null || echo "")

if [ -z "$EXISTING_MIGRATIONS" ]; then
    echo "No existing migrations to check."
    exit 0
fi

# Check if any existing migrations were modified
MODIFIED=0
for migration in $EXISTING_MIGRATIONS; do
    if git diff --name-only "$BASE" -- "$migration" | grep -q .; then
        echo "ERROR: Migration file was modified: $migration"
        echo "       Migrations are immutable. Create a new migration instead."
        MODIFIED=1
    fi
done

if [ $MODIFIED -eq 1 ]; then
    echo ""
    echo "To add a new migration, create a file like:"
    echo "  migrations/$(date +%Y%m%d%H%M%S)_description.sql"
    exit 1
fi

echo "Migration immutability check passed."
