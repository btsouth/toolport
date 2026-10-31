import assert from "node:assert/strict";
import test from "node:test";
import ts from "typescript";
import {
  checkCatalogs,
  checkMessageIntegrity,
  checkRustSource,
  checkSource,
  isCoverageGap,
  isMarkedType,
  parseCatalog,
} from "./check-locales.mjs";

const messages = new Set(["Hello", "Welcome {name}", "Ready"]);
const sourceErrors = (source) => checkSource(source, "fixture.tsx", messages);

test("JSON duplicate keys are rejected before JSON.parse can overwrite them", () => {
  const errors = [];
  parseCatalog('{"Hello":"Hello","Hello":"different"}', "common/en", errors);
  assert.match(errors.join("\n"), /duplicate key Hello/);
});

test("malformed catalogs and non-string values are rejected", () => {
  for (const source of ["[]", "null", "{", '{"Hello":3}', '{"Hello":""}']) {
    const errors = [];
    parseCatalog(source, "common/en", errors);
    assert.ok(errors.length, source);
  }
});

test("duplicates across groups are rejected for every locale, including unknown keys", () => {
  const catalogs = {
    common: { en: { Hello: "Hello" }, ja: { Hello: "こんにちは", extra: "余分" } },
    shell: { en: { Ready: "Ready" }, ja: { Ready: "準備完了", extra: "その他" } },
  };
  const { errors } = checkCatalogs(catalogs, ["en", "ja"], ["common", "shell"]);
  assert.ok(
    errors.some((error) => /shell\/ja: duplicate key across groups: extra/.test(error)),
  );
  catalogs.shell.en.Hello = "Hello";
  assert.match(
    checkCatalogs(catalogs, ["en", "ja"], ["common", "shell"]).errors.join("\n"),
    /shell\/en: duplicate key across groups: Hello/,
  );
});

test("catalog parity and interpolation names and occurrence counts are checked", () => {
  const catalogs = {
    common: {
      en: { "Welcome {name}": "Welcome {name}", Ready: "Ready" },
      ja: { "Welcome {name}": "ようこそ {other}", extra: "追加" },
    },
  };
  const errors = checkCatalogs(catalogs, ["en", "ja"], ["common"]).errors.join("\n");
  assert.match(errors, /invalid placeholders/);
  assert.match(errors, /missing translation Ready/);
  assert.match(errors, /unknown key extra/);
  catalogs.common.ja["Welcome {name}"] = "{name} {name}";
  assert.match(
    checkCatalogs(catalogs, ["en", "ja"], ["common"]).errors.join("\n"),
    /invalid placeholders/,
  );
});

test("calls, msg definitions and Trans literals require catalog entries", () => {
  for (const source of [
    't("Missing")',
    'translate("Missing")',
    'msg("Missing")',
    '<Trans message="Missing" />',
    '<Trans message={ok ? "Hello" : "Missing"} />',
  ]) {
    assert.match(
      sourceErrors(source).join("\n"),
      /missing source message "Missing"/,
      source,
    );
  }
  assert.deepEqual(
    sourceErrors(
      'const label = msg("Hello"); const ui = <Trans message="Hello" />; t(ok ? "Ready" : "Hello");',
    ),
    [],
  );
});

test("unmarked dynamic messages cannot silently escape coverage", () => {
  for (const source of [
    "t(label)",
    "translate(meta.label)",
    "<Trans message={label} />",
    "msg(prefix + suffix)",
  ]) {
    assert.match(
      sourceErrors(source).join("\n"),
      /dynamic source message must come from msg/,
      source,
    );
  }
});

test("SourceMessage brand requires every possible dynamic value to be marked", () => {
  const file = "/virtual/messages.ts";
  const source = `declare const sourceMessage: unique symbol;
    type SourceMessage = string & { readonly [sourceMessage]: true };
    declare const marked: SourceMessage;
    declare const mixed: SourceMessage | string;
    declare const anything: any;
    declare function t(value: string): string;
    t(marked); t(mixed); t(anything);`;
  const host = ts.createCompilerHost({});
  const original = host.getSourceFile.bind(host);
  host.getSourceFile = (name, ...args) =>
    name === file
      ? ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true)
      : original(name, ...args);
  const program = ts.createProgram([file], { noLib: true }, host);
  const tree = program.getSourceFile(file);
  const checker = program.getTypeChecker();
  assert.equal(checkSource(tree, file, messages, checker).length, 2);
  assert.equal(
    isMarkedType(
      checker.getTypeAtLocation(tree.statements[2].declarationList.declarations[0].name),
    ),
    true,
  );
});

test("JSX text and human-facing attributes require translation but technical attributes do not", () => {
  for (const source of [
    "<button>Hello</button>",
    '<div>{"Hello"}</div>',
    '<input placeholder="Hello" />',
    '<img alt="Hello" />',
    '<div aria-label={ok ? "Hello" : "Ready"} />',
  ]) {
    assert.match(sourceErrors(source).join("\n"), /unlocalized/, source);
  }
  assert.deepEqual(
    sourceErrors(
      '<div id="technical" className="flex" data-testid="hello" role="button"><input type="text" value="server-id" />{t("Hello")}<span>→</span></div>',
    ),
    [],
  );
});

test("translated comparison values and element identifiers are rejected", () => {
  assert.match(
    sourceErrors('if (t("Hello") === value) {}').join("\n"),
    /comparison values/,
  );
  assert.match(sourceErrors('<div key={t("Hello")} />').join("\n"), /identifiers stable/);
});

test("Rust tr/trf calls require cataloged literals, including raw and escaped strings", () => {
  assert.deepEqual(
    checkRustSource(
      'i18n::tr("Hello"); trf(r#"Welcome {name}"#, args); tr("\\u{48}ello");',
      "native.rs",
      messages,
    ),
    [],
  );
  assert.match(
    checkRustSource('tr("Missing")', "native.rs", messages).join("\n"),
    /missing source message/,
  );
  for (const source of [
    "tr(label)",
    'trf(format!("Hello"), args)',
    'tr("Hello".to_string())',
  ])
    assert.match(
      checkRustSource(source, "native.rs", messages).join("\n"),
      /must be a string literal/,
    );
});

test("Rust comments, string contents and function declarations are not translation calls", () => {
  const source =
    '// tr("Missing")\n/* nested /* tr("Missing") */ comment */\nconst DEMO: &str = r###"tr("Missing")"###; fn tr(message: &str) {}';
  assert.deepEqual(checkRustSource(source, "native.rs", messages), []);
});

test("message boundaries preserve intentional source whitespace", () => {
  assert.deepEqual(checkMessageIntegrity(" Hello\n", " Bonjour\n"), []);
  for (const [key, value] of [
    ["Hello", " Bonjour"],
    ["Hello", "Bonjour "],
    [" Hello", "Bonjour"],
    ["Hello\n", "Bonjour "],
  ])
    assert.match(checkMessageIntegrity(key, value).join("\n"), /whitespace mismatch/);
});

test("translations cannot add hidden characters", () => {
  // Machine translation leaves zero-width spaces next to code, so a copied command breaks.
  for (const [key, value, codePoint] of [
    ["Run sudo dnf install", "\u200B\u200B运行 sudo dnf install", "200B"],
    ["Open settings", "Abrir\u200B configurações", "200B"],
    ["Save", "\uFEFFSpeichern", "FEFF"],
    ["Save", "\u202ESpeichern", "202E"],
    ["Save", "Speichern\u2066", "2066"],
    ["Copy \u200Bthis", "Copier \u200Bceci\u200B", "200B"],
  ])
    assert.match(
      checkMessageIntegrity(key, value).join("\n"),
      new RegExp(`hidden character U\\+${codePoint}\\b`),
    );
  // Typography a language needs, and characters the source already carries, stay allowed.
  for (const [key, value] of [
    ["Configuration", "Konfigu\u00ADration"],
    ["Toolport app", "Toolport\u2060-App"],
    ["Team \u{1F469}\u200D\u{1F4BB}", "Équipe \u{1F469}\u200D\u{1F4BB}"],
    ["Copy \u200Bthis", "Copier \u200Bceci"],
  ])
    assert.deepEqual(checkMessageIntegrity(key, value), []);
});

test("versions, URLs, code, paths and protocol methods survive translation verbatim", () => {
  const samples = [
    ["Toolport v{version}", "Toolport gegen {version}"],
    ["Open https://example.com/mcp.", "Ouvrir https://example.fr/mcp."],
    ["Use `npm run start`", "Utiliser `npm commencer`"],
    ["Choose ~/projects/my-app", "Choisir ~/projets/mon-app"],
    ["Use /path/to/project", "Utiliser /chemin/vers/projet"],
    ["Run ./<file>.deb", "Lancer ./<fichier>.deb"],
    ["Read tools/list twice: tools/list", "Lire tools/list"],
    ["Open https://example.com/mcp", "Ouvrir https://example.com/mcp-renamed"],
    ["Use ~/project", "Utiliser ~/project-copy"],
    ["Toolport v{version}", "Toolport vv{version}"],
    ['Run "/Applications/My App.app/tool"', 'Lancer "/Applications/My App.app/outil"'],
  ];
  for (const [key, value] of samples)
    assert.match(
      checkMessageIntegrity(key, value).join("\n"),
      /changed technical literal/,
      key,
    );
  assert.deepEqual(
    checkMessageIntegrity(
      "Open https://example.com/mcp.",
      "Ouvrir https://example.com/mcp。",
    ),
    [],
  );
  assert.deepEqual(checkMessageIntegrity("Ready v{version}", "v{version} prêt"), []);
});

test("LLM token terms cannot become crypto coins or auth tokens", () => {
  for (const word of ["代币", "代幣", "權杖", "权杖", "令牌"])
    assert.match(
      checkMessageIntegrity("Net tokens saved", word).join("\n"),
      /banned LLM-token/,
    );
  assert.deepEqual(checkMessageIntegrity("Bearer token", "持有者令牌"), []);
  assert.deepEqual(checkMessageIntegrity("Access token", "存取權杖"), []);
  assert.deepEqual(checkMessageIntegrity("Saved auth token", "已保存认证令牌"), []);
  assert.deepEqual(checkMessageIntegrity("Tokens saved", "節省的 Token"), []);
});

test("GTK binding helpers validate their original source keys", () => {
  for (const source of [
    'l10n::label("Missing")',
    'l10n::bind_label(&label, "Missing")',
    'l10n::bind_dialog_response(&dialog, "ok", "Missing")',
    'msg("Missing")',
  ])
    assert.match(
      checkRustSource(source, "native.rs", messages).join("\n"),
      /missing source message/,
    );
  const collected = new Set();
  checkRustSource('l10n::button("Hello")', "native.rs", messages, { collect: collected });
  assert.deepEqual([...collected], ["Hello"]);
});

test("Rust conditional messages validate every result branch", () => {
  const valid =
    'trf(if count == 1 { msg("Hello") } else { crate::i18n::msg("Hello") }, values)';
  assert.deepEqual(checkRustSource(valid, "native.rs", messages), []);
  assert.match(
    checkRustSource(
      'trf(if one { msg("Hello") } else { msg("Missing") }, values)',
      "native.rs",
      messages,
    ).join("\n"),
    /missing source message/,
  );
  assert.match(
    checkRustSource(
      'tr(if one { msg("Hello") } else { backend_text })',
      "native.rs",
      messages,
    ).join("\n"),
    /must be a string literal/,
  );
});

test("coverage gaps only warn, while broken catalogs and misuse still fail", () => {
  const catalogs = {
    common: {
      en: { "Welcome {name}": "Welcome {name}", Ready: "Ready" },
      ja: { "Welcome {name}": "ようこそ {other}" },
    },
  };
  const catalogErrors = checkCatalogs(catalogs, ["en", "ja"], ["common"]).errors;
  const gaps = [
    ...catalogErrors.filter((error) => /missing translation Ready/.test(error)),
    ...sourceErrors('t("Not in the catalog yet")'),
    ...sourceErrors("<p>Plain text</p>"),
    ...sourceErrors("t(label)"),
  ];
  assert.equal(gaps.length, 4);
  for (const gap of gaps) assert.ok(isCoverageGap(gap), gap);
  const broken = [
    ...catalogErrors.filter((error) => /invalid placeholders/.test(error)),
    ...sourceErrors('<div key={t("Hello")} />'),
    ...sourceErrors('if (t("Hello") === value) {}'),
    "common/de: missing catalog",
  ];
  assert.equal(broken.length, 4);
  for (const error of broken) assert.ok(!isCoverageGap(error), error);
});

test("a message key that looks like a gap cannot downgrade a real error", () => {
  const catalogs = {
    common: {
      en: { "Error: missing {name}": "Error: missing {name}", catalog: "catalog" },
      ja: { "Error: missing {name}": "エラー: {other} がありません" },
    },
  };
  const errors = checkCatalogs(catalogs, ["en", "ja"], ["common"]).errors;
  const placeholder = errors.find((error) => /invalid placeholders/.test(error));
  assert.ok(placeholder);
  assert.ok(!isCoverageGap(placeholder), placeholder);
  assert.ok(errors.some((error) => error === "common/ja: missing translation catalog"));
  assert.ok(isCoverageGap("common/ja: missing translation catalog"));
  assert.ok(!isCoverageGap("common/ja: missing catalog"));
  assert.ok(!isCoverageGap("src/a.tsx:3: translate display text, not comparison values"));
});
