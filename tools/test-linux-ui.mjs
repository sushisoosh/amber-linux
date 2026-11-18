import { build } from "esbuild";
import { spawnSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const directory = await mkdtemp(join(tmpdir(), "amber-ui-tests-"));
try {
  const suites = ["linux-ui", "testing-ui"];
  await build({
    absWorkingDir: root,
    entryPoints: suites.map((suite) => `src/settings/${suite}.test.tsx`),
    outdir: directory,
    outExtension: { ".js": ".cjs" },
    bundle: true,
    platform: "node",
    format: "cjs",
    target: "node22",
    jsx: "automatic",
    tsconfig: "tsconfig.json",
    sourcemap: "inline",
  });
  const result = spawnSync(process.execPath, ["--enable-source-maps", "--test", ...suites.map((suite) => join(directory, `${suite}.test.cjs`))], { cwd: root, stdio: "inherit" });
  if (result.error) throw result.error;
  process.exitCode = result.status ?? 1;
} finally {
  await rm(directory, { recursive: true, force: true });
}
