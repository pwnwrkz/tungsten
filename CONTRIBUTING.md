# Contributing to Tungsten

Thanks for your interest in contributing to Tungsten! Before that, please read through this guide before opening a PR or issue.

-----

## Table of Contents

- [Getting Started](#getting-started)
- [Branch Naming](#branch-naming)
- [Commit Messages](#commit-messages)
- [Pull Requests](#pull-requests)
- [AI Usage Policy](#ai-usage-policy)

-----

## Getting Started

1. Fork the repository and clone it locally.
1. Make sure you have [Rust](https://www.rust-lang.org/tools/install) installed.
1. Build the project:
   
   ```sh
   cargo build
   ```
1. Run tests before making changes to ensure everything passes:
   
   ```sh
   cargo test
   ```
1. Create a new branch from `main` before making any changes (see [Branch Naming](#branch-naming)).

-----

## Branch Naming

Branches should follow this pattern:

```
<type>/<short_description>
```

Use lowercase and underscores, no spaces or special characters.

|Type      |When to use                             |
|----------|----------------------------------------|
|`feat`    |New feature                             |
|`fix`     |Bug fix                                 |
|`docs`    |Documentation updates                   |
|`refactor`|Code restructure with no behavior change|
|`test`    |Adding or updating tests                |
|`chore`   |Tooling, dependencies, config           |
|`ci`      |CI/CD changes                           |

**Examples:**

```
feat/spritesheet_padding_option
fix/asset_upload_retry_logic
docs/update_wiki_links
chore/bump_dependencies
refactor/asset_resolver_cleanup
```

-----

## Commit Messages

This project follows the [Conventional Commits](https://www.conventionalcommits.org/) specification.

### Format

```
<type>(<scope>): <short description>
```

- **type**: one of the types listed below
- **scope**: optional, the area of the codebase affected (e.g. `cli`, `sync`, `spritesheet`, `upload`)
- **description**: brief summary in imperative mood, lowercase, no trailing period

### Types

|Type      |When to use                               |
|----------|------------------------------------------|
|`feat`    |New feature                               |
|`fix`     |Bug fix                                   |
|`docs`    |Documentation changes                     |
|`style`   |Formatting, whitespace (no logic change)  |
|`refactor`|Code restructure (no feature/fix)         |
|`test`    |Adding or updating tests                  |
|`chore`   |Build process, dependency updates, tooling|
|`perf`    |Performance improvements                  |
|`ci`      |CI/CD configuration changes               |
|`revert`  |Reverting a previous commit               |

### Rules

- Use **imperative mood**: “add feature” not “added feature”
- Keep the subject line **under 72 characters**
- **No period** at the end of the subject line
- **Lowercase** everything (except proper nouns)
- Be specific, `fix asset upload failing on retry after 429` instead of `fix upload`

### Examples

```
feat(spritesheet): add configurable padding between packed sprites
fix(upload): handle rate limit retry on Roblox API 429 response
docs(readme): fix broken wiki link in getting started section
refactor(sync): simplify asset path resolution logic
chore(deps): update image crate to 0.25
test(cli): add integration test for sync command with mock assets
```

### Extended commit body (for larger changes)

When a change needs more context, add a body after a blank line:

```
feat(spritesheet): add configurable padding between packed sprites

Adds a `padding` field to the Tungsten config that controls the pixel
gap between each sprite in the packed sheet. Defaults to 0 to preserve
existing behavior.

Closes #18
```

-----

## Pull Requests

- Keep PRs focused, one feature or fix per PR.
- Reference any related issues in the PR description (e.g. `Closes #42`).
- Ensure `cargo test` and `cargo clippy` pass before requesting a review.
- Provide a clear summary of *what* changed and *why*.

-----

## AI Usage Policy

AI-assisted contributions are welcome, provided they meet the following conditions in the [policy](AI_POLICY.md).

-----

*Thanks again for contributing to Tungsten!*