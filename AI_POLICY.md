# AI Usage Policy

This document outlines the rules for AI-assisted contributions to Tungsten.
AI tools are permitted, but with clear boundaries to maintain code quality, ownership, and repository integrity.

---

## Table of Contents

- [What's Allowed](#whats-allowed)
- [Code Ownership](#code-ownership)
- [Prompt & Instruction Files](#prompt--instruction-files)
- [Disclosure](#disclosure)

---

## What's Allowed

You may use AI tools (e.g. GitHub Copilot, ChatGPT, Claude, Cursor) to:

- Assist in writing or refactoring code
- Help debug issues or understand error messages
- Draft documentation, comments, or commit messages
- Suggest implementations or approaches

AI is a tool to help you move faster, not a replacement for understanding what you're contributing.

---

## Code Ownership

This is the most important rule: **you are fully responsible for every line you submit.**

- You must be able to read, explain, and defend any AI-generated code in your PR.
- Do not submit code you don't understand just because an AI wrote it.
- AI-generated code is held to the **exact same quality standard** as hand-written code: idiomatic Rust, well-tested, and consistent with the codebase.
- If a reviewer asks you to explain your code, "the AI wrote it" is not an acceptable answer.

> When in doubt, take the time to understand it before submitting.

---

## Prompt & Instruction Files

**AI prompt and instruction files must never be committed to this repository.** This includes, but is not limited to:

| File                              | Tool                          |
| --------------------------------- | ----------------------------- |
| `CLAUDE.md`                       | Claude / Anthropic            |
| `AGENTS.md`                       | OpenAI Codex / general agents |
| `.cursorrules`                    | Cursor                        |
| `.github/copilot-instructions.md` | GitHub Copilot                |
| `copilot-instructions.md`         | GitHub Copilot                |
| `.aider*`                         | Aider                         |
| `GEMINI.md`                       | Gemini CLI                    |

These are personal configuration files for your local AI setup and have no place in the project repository. Either delete them when you're done, or add them to `.git/info/exclude` to keep them out of commits without touching any shared files.

If you're unsure whether a file falls into this category, it probably does. Don't commit it.

---

## Disclosure

If a significant portion of your contribution was AI-assisted, note it briefly in your PR description. A single line is enough:

```
AI assistance: Used Claude to help scaffold the initial implementation of X.
```

This isn't about policing tools, it's about transparency and helping reviewers give better feedback.

---

_If you have questions about this policy, open an issue or start a discussion._
