import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { withTako } from "tako.sh/nextjs";

const projectRoot = fileURLToPath(new URL(".", import.meta.url));

function findWorkspaceRoot(startDir) {
  let current = startDir;

  while (true) {
    const packageJsonPath = path.join(current, "package.json");
    if (existsSync(packageJsonPath)) {
      const packageJson = JSON.parse(readFileSync(packageJsonPath, "utf8"));
      if (packageJson.workspaces) {
        return current;
      }
    }

    const parent = path.dirname(current);
    if (parent === current) {
      return startDir;
    }
    current = parent;
  }
}

export default withTako({
  reactStrictMode: true,
  turbopack: {
    root: findWorkspaceRoot(projectRoot),
  },
});
