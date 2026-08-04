export async function formatUsageScript(code: string): Promise<string> {
  const [prettier, parserBabel, pluginEstree] = await Promise.all([
    import("prettier/standalone"),
    import("prettier/parser-babel"),
    import("prettier/plugins/estree"),
  ]);

  return prettier.format(code, {
    parser: "babel",
    plugins: [parserBabel.default, pluginEstree.default],
    semi: true,
    singleQuote: false,
    tabWidth: 2,
    printWidth: 80,
  });
}
