import { TAKO_D2_THEME_MARKER, TAKO_D2_THEME_SOURCE } from "../config/d2-theme.js";

export function remarkD2Theme() {
  return function transformer(tree) {
    visit(tree, (node) => {
      if (node?.type !== "code" || node.lang !== "d2" || typeof node.value !== "string") {
        return;
      }

      if (node.value.includes(TAKO_D2_THEME_MARKER)) {
        return;
      }

      node.value = `${TAKO_D2_THEME_SOURCE}\n${node.value}`;
    });
  };
}

function visit(node, visitor) {
  visitor(node);

  if (!node || typeof node !== "object" || !Array.isArray(node.children)) {
    return;
  }

  for (const child of node.children) {
    visit(child, visitor);
  }
}
