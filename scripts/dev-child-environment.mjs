// What a development launcher is allowed to hand down to the application it starts.
//
// `npm run dev:browser` and `npm run tauri:dev` both start from this process's own
// environment, which npm has already shaped for its own output and which
// `windowsNativeBuildEnvironment()` then reshapes for the MSYS2 toolchain. Two of
// those values are wrong for the application:
//
//   * `FORCE_COLOR` makes every plain Node command the app runs emit ANSI escapes,
//     which land verbatim in the terminal surface and in anything that reads a
//     command's output as text. It also suppresses the host's own `NO_COLOR=1`
//     default, because that default only applies when neither colour variable is
//     already present.
//
//   * The build `PATH` puts `<msys2>\mingw64\bin` and `<msys2>\usr\bin` *ahead* of
//     `System32`, which is what makes `gcc` resolve to the native compiler. The
//     application inherits that order and passes it to every shell it opens, where
//     a bare `cmd` then resolves to `<msys2>\usr\bin\cmd` — a bash script — instead
//     of `System32\cmd.exe`. `usr\bin` shadows `find`, `sort`, `more`, `link` and
//     `tar` the same way.
//
// The build environment still needs the toolchain first, so the launcher cannot
// simply stop prepending. Instead it carries the *application* PATH alongside the
// build PATH in a handoff variable, and the Rust entry point restores it before it
// creates any thread or child process (`child_environment.rs`). The toolchain
// directories are appended rather than dropped, so a GNU-target binary can still
// find its runtime DLLs while `System32` wins every name lookup.
//
// A user who configures `FORCE_COLOR` or `PATH` on one command inside the app is
// unaffected: this only reshapes what the launcher itself inherited.

/** Names removed from every development child environment, matched case-insensitively. */
const INHERITED_COLOUR_NAMES = new Set(["force_color"]);

/**
 * Handoff variable carrying the PATH the application should run with. The Rust
 * entry point consumes and deletes it, so it never reaches a user's shell.
 */
export const DEV_APPLICATION_PATH_ENVIRONMENT_NAME = "MEWORK_DEV_APPLICATION_PATH";

function environmentEntry(environment, name) {
  const key = Object.keys(environment).find(
    (candidate) => candidate.toLowerCase() === name
  );
  return key === undefined ? undefined : environment[key];
}

function deleteEnvironmentEntry(environment, name) {
  const lowered = name.toLowerCase();
  for (const key of Object.keys(environment)) {
    if (key.toLowerCase() === lowered) delete environment[key];
  }
}

/**
 * A copy of `environment` without the colour variables a launcher inherited.
 * The input object is never modified — `process.env` in particular stays intact.
 */
export function sanitizeDevChildEnvironment(environment) {
  const result = { ...environment };
  for (const key of Object.keys(result)) {
    // Windows environment names are case-insensitive, so `Force_Color` would
    // otherwise reach the child under a spelling an exact-name delete missed.
    if (INHERITED_COLOUR_NAMES.has(key.toLowerCase())) delete result[key];
  }
  return result;
}

/**
 * The PATH the application should run with: the priority it had before the
 * toolchain was injected, with the injected directories appended instead of
 * removed. Returns `undefined` when the build did not change PATH at all.
 */
export function devApplicationPath(applicationPath, buildPath) {
  if (typeof applicationPath !== "string" || typeof buildPath !== "string") {
    return undefined;
  }
  if (applicationPath === buildPath) return undefined;
  const split = (value) => value.split(";").map((entry) => entry.trim()).filter(Boolean);
  const applicationEntries = split(applicationPath);
  const owned = new Set(applicationEntries.map((entry) => entry.toLowerCase()));
  const appended = split(buildPath).filter((entry) => !owned.has(entry.toLowerCase()));
  if (appended.length === 0) return undefined;
  return [...applicationEntries, ...appended].join(";");
}

/**
 * The environment a development launcher hands to the application: the build
 * environment with the inherited colour variables removed and, on Windows, the
 * application PATH carried alongside for the Rust entry point to restore.
 */
export function devChildEnvironment({
  buildEnvironment,
  applicationEnvironment = process.env,
  platform = process.platform
}) {
  const result = sanitizeDevChildEnvironment(buildEnvironment);
  // A stale marker inherited from an outer launcher must never survive: the
  // application would restore some other run's PATH.
  deleteEnvironmentEntry(result, DEV_APPLICATION_PATH_ENVIRONMENT_NAME);
  if (platform !== "win32") return result;
  const applicationPath = devApplicationPath(
    environmentEntry(applicationEnvironment, "path"),
    environmentEntry(result, "path")
  );
  if (applicationPath !== undefined) {
    result[DEV_APPLICATION_PATH_ENVIRONMENT_NAME] = applicationPath;
  }
  return result;
}
