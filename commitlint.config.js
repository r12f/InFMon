// commitlint configuration for InFMon.
// Enforces Conventional Commits (https://www.conventionalcommits.org/).
// The scope list is seeded from specs/001-ci-and-precommit.md §3.
module.exports = {
  extends: ["@commitlint/config-conventional"],
  rules: {
    "type-enum": [
      2,
      "always",
      [
        "feat",
        "fix",
        "perf",
        "refactor",
        "docs",
        "test",
        "build",
        "ci",
        "chore",
        "style",
        "revert",
      ],
    ],
    // scope-enum is intentionally a warning (severity 1) so contributors can
    // introduce new scopes without a config PR; the listed scopes are the
    // canonical set from specs/001-ci-and-precommit.md §3 and unfamiliar
    // scopes will surface a yellow notice in commitlint output without
    // blocking the commit. Promote to 2 if/when we want a hard allowlist.
    "scope-enum": [
      1,
      "always",
      ["backend", "frontend", "cli", "tests", "ci", "specs", "deps", "release"],
    ],
    "subject-case": [0],
    "header-max-length": [2, "always", 100],
    "body-max-line-length": [1, "always", 120],
  },
};
