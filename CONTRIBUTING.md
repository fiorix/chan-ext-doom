# Contributing

Keep changes narrow, use conventional commit messages, stage explicit pathspecs, review the staged diff, and run the relevant repository checks before committing.

## Documentation

- Describe the repository as it works now. Do not organize reference documentation around staged delivery labels, task or review language, or development-process framing.
- Keep development history only in `CHANGELOG.md`.
- Do not use em dashes. Prefer a colon, period, comma, or parentheses.
- Add a Mermaid diagram when it makes a multi-component relationship or sequence materially easier to understand. Keep diagrams small and factual.
- Keep each prose paragraph and list item on one logical line. Tables, code fences, license text, and upstream files retain their native formatting.
- Do not rewrite upstream `engine/README.md`, `engine/COPYING.md`, or license texts to match repository-authored style.

## Checks

Run the Rust gate for workspace changes:

```sh
./scripts/gate.sh
```

Run the engine tests for changes under `engine/`:

```sh
cd engine
npm test
```

Run the keyboard relay test for changes to Chan's keyboard relay in `crates/doom-extension/assets/frame.html` or `app.js`. It executes the game frame's relay block and the extension page's message listener as shipped, against keydowns from published keyboard layouts and stale frames:

```sh
node --test scripts/tests/keyboard-relay.test.mjs
```

Engine build and artifact verification commands are documented in [`engine/docs/provenance.md`](engine/docs/provenance.md).

## Releases

Release tags must match the workspace version as `vMAJOR.MINOR.PATCH`. A tag runs the native Linux x86_64, Linux arm64, Windows x86_64, and macOS arm64 builds, packages the tracked runtime and corresponding engine source, and publishes the archives with `install.sh` and `SHA256SUMS`. Run the release workflow manually to exercise the build matrix without publishing a release.
