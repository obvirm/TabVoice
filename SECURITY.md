# Security Policy

## Supported Versions

Currently, only the latest release of TabVoice is supported with security updates. 

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | Yes                |
| Older   | No                 |

## Reporting a Vulnerability

If you discover a security vulnerability within TabVoice, please do not open a public issue. Instead, please send an email to the repository maintainer directly or use GitHub's private vulnerability reporting feature if enabled.

We will review the vulnerability and get back to you with a patch and release timeline. We ask that you give us reasonable time to fix the issue before publicizing it.

## Scope

Security issues include:
- Unintended execution of commands on the host machine.
- Unauthorized reading or writing of files outside the configured paths.
- Local privilege escalation via the application.
- Exposing the user's keystrokes to third-party endpoints.

Note that TabVoice relies heavily on global hotkeys and auto-pasting (simulated keyboard events). This is the intended behavior of the application and not considered a vulnerability. TabVoice runs models entirely locally and does not upload any audio data to external servers.
