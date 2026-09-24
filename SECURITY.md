# Security

OpenAtelier opens media files and project files from anywhere, and loads plugins, so
reports of crashes or worse from crafted files are taken seriously.

## Reporting a vulnerability

Please **don't open a public issue**. Use GitHub's
[private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
(the repository's **Security** tab → **Report a vulnerability**) with the steps to
reproduce and, if you can, a file that triggers it. You'll get an answer as soon as the
maintainers can look at it, and credit in the fix unless you'd rather not.

## Scope notes

- Plugins are folders of WGSL shaders and sound shaders; neither can run native code,
  read files or reach the network. A way around that is a vulnerability.
- Media is probed and some of it decoded by `ffmpeg`/`ffprobe` from your `PATH`; issues
  in those belong upstream, but tell us if OpenAtelier makes them easier to reach.
- The optional caption engine is downloaded only when asked for, from uv's GitHub
  releases, PyPI (faster-whisper and its dependencies) and Hugging Face (the models),
  into its own folder. Problems in how OpenAtelier fetches or runs it are in scope.
- Only the latest `main` is supported while the project is pre-1.0.
