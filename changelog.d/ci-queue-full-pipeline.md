### Changed

- A pull request runs a quick CI profile (unit tests in a debug build, fmt, lint, script tests and semver), and the merge queue, main and a manual dispatch run the full pipeline; ci-ok refuses a missing or invalid plan and any failed, cancelled, missing or unexpectedly skipped job.
