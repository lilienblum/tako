export const SNIPPET_THEME = {
  name: "tako-sunrise",
  type: "light",
  colors: {
    "editor.background": "#F7F1EA",
    "editor.foreground": "#2F2A44",
    "editorLineNumber.foreground": "#9188A6",
    "editor.selectionBackground": "#F4D6D1",
    "editorCursor.foreground": "#2F2A44",
    "editorIndentGuide.background": "#E8DFD7",
  },
  tokenColors: [
    {
      scope: ["comment", "punctuation.definition.comment"],
      settings: {
        foreground: "#8A809E",
        fontStyle: "italic",
      },
    },
    {
      scope: ["keyword", "storage", "storage.type"],
      settings: {
        foreground: "#D06F6B",
      },
    },
    {
      scope: ["string", "string.quoted", "string.template"],
      settings: {
        foreground: "#2D8D67",
      },
    },
    {
      scope: ["constant.numeric", "constant.character", "constant.language"],
      settings: {
        foreground: "#C86A4E",
      },
    },
    {
      scope: ["entity.name.function", "support.function", "meta.function-call"],
      settings: {
        foreground: "#2A67D4",
      },
    },
    {
      scope: ["entity.name.type", "support.type", "storage.type.class"],
      settings: {
        foreground: "#7D4CA8",
      },
    },
    {
      scope: ["operator", "keyword.operator"],
      settings: {
        foreground: "#1B8A98",
      },
    },
    {
      scope: ["variable", "identifier"],
      settings: {
        foreground: "#2F2A44",
      },
    },
    {
      scope: ["punctuation", "meta.brace", "delimiter"],
      settings: {
        foreground: "#766E87",
      },
    },
    {
      scope: ["invalid"],
      settings: {
        foreground: "#FFF9F4",
        background: "#C83A3A",
      },
    },
  ],
};
