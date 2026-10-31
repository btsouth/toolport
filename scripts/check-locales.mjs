#!/usr/bin/env node
// Check catalog integrity and statically marked UI messages before shipping.
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

export const languages = ["en", "ja", "zh-CN", "zh-TW", "ko", "es", "fr", "de", "pt-BR"];
export const groups = ["common", "shell", "connections", "workflows"];
const translationFunctions = new Set(["t", "tr", "translate", "msg"]);
const displayAttributes = new Set([
  "title",
  "placeholder",
  "aria-label",
  "aria-description",
  "alt",
  "label",
  "description",
  "message",
  "tooltip",
  "helperText",
  "emptyLabel",
]);
const hasWords = (value) => /\p{L}/u.test(value);
const placeholders = (value) =>
  [...value.matchAll(/\{([a-zA-Z][a-zA-Z0-9_]*)\}/g)]
    .map((match) => match[1])
    .sort()
    .join(",");
// Zero-width spaces, BOMs and bidi embedding, override and isolate controls have no place
// in a translation unless the source carries them. Soft hyphens and joiners stay allowed.
const hiddenCharacters = /[\u200B\uFEFF\u202A-\u202E\u2066-\u2069]/gu;
const addedHiddenCharacters = (key, value) => {
  const counts = (text) => {
    const result = new Map();
    for (const [character] of text.matchAll(hiddenCharacters))
      result.set(character, (result.get(character) ?? 0) + 1);
    return result;
  };
  const source = counts(key);
  return [...counts(value)]
    .filter(([character, count]) => count > (source.get(character) ?? 0))
    .map(([character]) => character);
};

// Auth tokens are credentials; all other token messages describe model usage.
export function isLlmTokenMessage(key) {
  if (!/\btokens?\b/i.test(key)) return false;
  if (
    /tokens saved|savings|baseline|estimate|schemas?|agent context|fewer tokens|model searched/i.test(
      key,
    )
  )
    return true;
  return (
    !/^(Token|Copy token|New token|Hide token|Show token|Remove token|Store token|Paste a new token|Copy this token now|Copy this token now, it won't be shown again\.|Reveal token)$/i.test(
      key.trim(),
    ) &&
    !/auth|bearer|access token|sign-in|keychain|credential|private|clipboard|copied|copy the|token endpoint|HTTP (endpoint|client) token|team token|each gets|disconnect this token|token removal|token update|paste|storing|browser|provider|requests? a token|get a token/i.test(
      key,
    )
  );
}

// Extract only technical literals, not ordinary prose containing a slash.
export function protectedLiterals(key) {
  const patterns = [
    /v\{[a-zA-Z][a-zA-Z0-9_]*\}/g,
    /`[^`]+`/g,
    /(?<=")(?:\/|~\/|[A-Za-z]:\\)[^"]+(?=")/g,
    /\btoolport\.app(?:\/[^\s"']*)?/g,
    /https?:\/\/[^\s<>"']*/g,
    /(?:~\/|\.\/|\/[A-Za-z][A-Za-z0-9_.-]*\/|[A-Za-z]:\\)[^\s"']*/g,
    /\b(?:tools|resources|prompts)\/[a-z_]+\b/g,
  ];
  return [
    ...new Set(
      patterns.flatMap((pattern) =>
        [...key.matchAll(pattern)].map((match) => match[0].replace(/[.,;:!?)]*$/, "")),
      ),
    ),
  ].filter(Boolean);
}

export function checkMessageIntegrity(key, value) {
  const errors = [];
  const edges = (text) => [text.match(/^\s*/u)[0], text.match(/\s*$/u)[0]];
  if (JSON.stringify(edges(key)) !== JSON.stringify(edges(value)))
    errors.push("whitespace mismatch");
  if (placeholders(key) !== placeholders(value)) errors.push("invalid placeholders");
  for (const character of addedHiddenCharacters(key, value))
    errors.push(
      `hidden character U+${character.codePointAt(0).toString(16).toUpperCase().padStart(4, "0")}`,
    );
  for (const literal of protectedLiterals(key)) {
    // A preserved prefix is not the same URL/path (example.com.evil or
    // ~/project-renamed). Protocol-only examples deliberately allow a suffix.
    const count = (text) => {
      let total = 0;
      for (
        let at = text.indexOf(literal);
        at !== -1;
        at = text.indexOf(literal, at + literal.length)
      ) {
        const after = text.slice(at + literal.length);
        const extended =
          !literal.startsWith("`") &&
          !literal.endsWith("://") &&
          !/^(?:tools|resources|prompts)\//.test(literal) &&
          /^(?:[A-Za-z0-9_/~\\<>@%+?=#&:-]|\.[A-Za-z0-9])/u.test(after);
        const prefixedVersion =
          /^v\{/.test(literal) && /[A-Za-z0-9_]$/.test(text.slice(0, at));
        if (!extended && !prefixedVersion) total++;
      }
      return total;
    };
    if (count(key) !== count(value)) errors.push(`changed technical literal ${literal}`);
  }
  if (isLlmTokenMessage(key) && /代币|代幣|權杖|权杖|令牌/u.test(value))
    errors.push("banned LLM-token term");
  return errors;
}

export function parseCatalog(source, name, errors) {
  const tree = ts.parseJsonText(name, source);
  const seen = new Set();
  function visit(node) {
    if (ts.isPropertyAssignment(node)) {
      const key = node.name.text;
      if (seen.has(key)) errors.push(`${name}: duplicate key ${key}`);
      seen.add(key);
    }
    ts.forEachChild(node, visit);
  }
  visit(tree);
  try {
    const value = JSON.parse(source);
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("catalog must be an object");
    for (const [key, message] of Object.entries(value)) {
      if (!key.trim() || typeof message !== "string" || !message.trim())
        errors.push(`${name}: invalid message ${JSON.stringify(key)}`);
    }
    return value;
  } catch (error) {
    errors.push(`${name}: invalid JSON: ${error.message}`);
    return {};
  }
}

export function checkCatalogs(catalogs, localeNames = languages, groupNames = groups) {
  const errors = [];
  const messages = new Set();
  for (const language of localeNames) {
    const owners = new Map();
    for (const group of groupNames) {
      const catalog = catalogs[group]?.[language];
      if (!catalog) {
        errors.push(`${group}/${language}: missing catalog`);
        continue;
      }
      const english = catalogs[group]?.en ?? {};
      for (const [key, value] of Object.entries(catalog)) {
        if (owners.has(key))
          errors.push(
            `${group}/${language}: duplicate key across groups: ${key} (also ${owners.get(key)})`,
          );
        owners.set(key, group);
        if (typeof value !== "string" || !value.trim()) {
          errors.push(`${group}/${language}: invalid message ${key}`);
          continue;
        }
        if (language === "en") {
          messages.add(key);
          if (key !== value || !key.trim())
            errors.push(`${group}/en: source and value must match: ${key}`);
        } else if (!Object.hasOwn(english, key)) {
          errors.push(`${group}/${language}: unknown key ${key}`);
        }
        if (language !== "en") {
          for (const error of checkMessageIntegrity(key, value))
            errors.push(`${group}/${language}: ${error} in ${key}`);
        }
      }
      for (const key of Object.keys(english)) {
        if (!Object.hasOwn(catalog, key))
          errors.push(`${group}/${language}: missing translation ${key}`);
      }
    }
  }
  return { errors, messages };
}

function callName(node) {
  if (!ts.isCallExpression(node)) return undefined;
  return ts.isIdentifier(node.expression) ? node.expression.text : undefined;
}

// TypeScript retains unique-symbol brand properties on intersections and unions.
// Requiring the brand closes the gap between msg-marked definitions and t(label).
export function isMarkedType(type) {
  if (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown | ts.TypeFlags.Never))
    return false;
  if (type.isUnion()) return type.types.every(isMarkedType);
  return type
    .getProperties()
    .some((property) => property.escapedName.toString().startsWith("__@sourceMessage@"));
}

export function checkSource(source, file, messages, checker) {
  const errors = [];
  const tree =
    typeof source === "string"
      ? ts.createSourceFile(
          file,
          source,
          ts.ScriptTarget.Latest,
          true,
          file.endsWith("tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
        )
      : source;
  const report = (node, message) => {
    const line = tree.getLineAndCharacterOfPosition(node.getStart(tree)).line + 1;
    errors.push(`${file}:${line}: ${message}`);
  };
  function checkMessage(node) {
    if (!node) return;
    if (
      ts.isParenthesizedExpression(node) ||
      ts.isAsExpression(node) ||
      ts.isSatisfiesExpression(node)
    )
      return checkMessage(node.expression);
    if (ts.isStringLiteralLike(node)) {
      if (!messages.has(node.text))
        report(node, `missing source message ${JSON.stringify(node.text)}`);
    } else if (ts.isConditionalExpression(node)) {
      checkMessage(node.whenTrue);
      checkMessage(node.whenFalse);
    } else if (callName(node) === "msg") {
      checkMessage(node.arguments[0]);
    } else if (!checker || !isMarkedType(checker.getTypeAtLocation(node))) {
      report(
        node,
        "dynamic source message must come from msg(...) with the SourceMessage brand",
      );
    }
  }
  function rawText(node) {
    if (ts.isStringLiteralLike(node) && hasWords(node.text))
      report(node, `unlocalized UI text ${JSON.stringify(node.text)}`);
    else if (ts.isConditionalExpression(node)) {
      rawText(node.whenTrue);
      rawText(node.whenFalse);
    } else if (
      ts.isParenthesizedExpression(node) ||
      ts.isAsExpression(node) ||
      ts.isSatisfiesExpression(node)
    )
      rawText(node.expression);
    else if (
      ts.isBinaryExpression(node) &&
      [
        ts.SyntaxKind.PlusToken,
        ts.SyntaxKind.AmpersandAmpersandToken,
        ts.SyntaxKind.BarBarToken,
        ts.SyntaxKind.QuestionQuestionToken,
      ].includes(node.operatorToken.kind)
    ) {
      rawText(node.left);
      rawText(node.right);
    } else if (
      ts.isTemplateExpression(node) &&
      [node.head.text, ...node.templateSpans.map((span) => span.literal.text)].some(
        hasWords,
      )
    )
      report(node, "unlocalized interpolated UI text");
  }
  function visit(node) {
    if (ts.isCallExpression(node) && translationFunctions.has(callName(node))) {
      if (!node.arguments[0]) report(node, "translation requires a source message");
      else checkMessage(node.arguments[0]);
    }
    if (ts.isJsxText(node) && hasWords(node.text))
      report(node, `unlocalized JSX text ${JSON.stringify(node.text.trim())}`);
    if (
      ts.isJsxExpression(node) &&
      node.expression &&
      (ts.isJsxElement(node.parent) || ts.isJsxFragment(node.parent))
    )
      rawText(node.expression);
    if (ts.isJsxAttribute(node) && node.initializer) {
      const name = node.name.getText(tree);
      const expression = ts.isJsxExpression(node.initializer)
        ? node.initializer.expression
        : node.initializer;
      if (displayAttributes.has(name) && expression) {
        const tag = node.parent.parent.tagName?.getText(tree);
        if (tag === "Trans" && name === "message") checkMessage(expression);
        else rawText(expression);
      }
      if (
        ["id", "key"].includes(name) &&
        expression &&
        translationFunctions.has(callName(expression))
      )
        report(node, "keep element identifiers stable across languages");
    }
    if (
      ts.isBinaryExpression(node) &&
      [
        ts.SyntaxKind.EqualsEqualsEqualsToken,
        ts.SyntaxKind.ExclamationEqualsEqualsToken,
        ts.SyntaxKind.EqualsEqualsToken,
        ts.SyntaxKind.ExclamationEqualsToken,
      ].includes(node.operatorToken.kind) &&
      [node.left, node.right].some((value) => translationFunctions.has(callName(value)))
    )
      report(node, "translate display text, not comparison values");
    ts.forEachChild(node, visit);
  }
  visit(tree);
  return errors;
}

// A small Rust lexer keeps comments and raw strings from producing false calls.
// Native translation calls use tr("source") or trf("source", parameters).
export function checkRustSource(source, file, messages, options = {}) {
  const tokens = [];
  const errors = [];
  let offset = 0;
  while (offset < source.length) {
    const rest = source.slice(offset);
    if (/^\s/.test(rest)) {
      offset++;
      continue;
    }
    if (rest.startsWith("//")) {
      const end = source.indexOf("\n", offset);
      offset = end < 0 ? source.length : end;
      continue;
    }
    if (rest.startsWith("/*")) {
      offset += 2;
      let depth = 1;
      while (offset < source.length && depth) {
        if (source.startsWith("/*", offset)) {
          depth++;
          offset += 2;
        } else if (source.startsWith("*/", offset)) {
          depth--;
          offset += 2;
        } else offset++;
      }
      continue;
    }
    const raw = /^r(#{0,255})"/.exec(rest);
    if (raw) {
      const endMarker = '"' + raw[1];
      const end = source.indexOf(endMarker, offset + raw[0].length);
      if (end < 0) break; // rustc reports unterminated source strings.
      tokens.push({
        text: source.slice(offset + raw[0].length, end),
        string: true,
        offset,
      });
      offset = end + endMarker.length;
      continue;
    }
    if (rest[0] === '"') {
      let end = offset + 1;
      while (end < source.length) {
        if (source[end] === "\\") end += 2;
        else if (source[end++] === '"') break;
      }
      const literal = source.slice(offset, end);
      let text;
      try {
        text = literal
          .slice(1, -1)
          .replace(
            /\\(u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|\r?\n\s*|[nrt0\\"'])/g,
            (_, escape) => {
              if (escape.startsWith("u{"))
                return String.fromCodePoint(
                  parseInt(escape.slice(2, -1).replaceAll("_", ""), 16),
                );
              if (escape.startsWith("x"))
                return String.fromCharCode(parseInt(escape.slice(1), 16));
              if (escape.includes("\n")) return "";
              return {
                n: "\n",
                r: "\r",
                t: "\t",
                0: "\0",
                "\\": "\\",
                '"': '"',
                "'": "'",
              }[escape];
            },
          );
      } catch {
        text = undefined;
      }
      tokens.push({ text, string: true, offset });
      offset = end;
      continue;
    }
    const character = /^'(?:\\.|[^'\\])'/u.exec(rest);
    if (character) {
      offset += character[0].length;
      continue;
    }
    const identifier = /^[a-zA-Z_][a-zA-Z0-9_]*/.exec(rest);
    const text = identifier?.[0] ?? rest[0];
    tokens.push({ text, offset });
    offset += text.length;
  }
  const helperArguments = {
    label: 0,
    button: 0,
    check_button: 0,
    formatted_label: 0,
    bind_label: 1,
    bind_text: 1,
    bind_formatted_text: 1,
    bind_formatted_label: 1,
    bind_formatted_button: 1,
    bind_button: 1,
    bind_check_button: 1,
    bind_tooltip: 1,
    bind_placeholder: 1,
    bind_formatted_dialog_heading: 1,
    bind_formatted_dialog_body: 1,
    bind_stack_page_title: 1,
    bind_window_title: 1,
    bind_dialog_heading: 1,
    bind_dialog_body: 1,
    bind_dialog_response: 2,
  };
  for (let index = 0; index < tokens.length; index++) {
    const token = tokens[index];
    const direct = ["tr", "trf", "msg"].includes(token.text);
    const qualifiedHelper =
      Object.hasOwn(helperArguments, token.text) &&
      tokens[index - 1]?.text === ":" &&
      tokens[index - 2]?.text === ":" &&
      ["l10n", "i18n"].includes(tokens[index - 3]?.text);
    if (
      token.string ||
      (!direct && !qualifiedHelper) ||
      tokens[index + 1]?.text !== "(" ||
      tokens[index - 1]?.text === "fn"
    )
      continue;
    let position = index + 2;
    for (
      let argument = 0;
      argument < (qualifiedHelper ? helperArguments[token.text] : 0);
      argument++
    ) {
      let depth = 0;
      while (position < tokens.length) {
        const value = tokens[position++];
        if (!value.string && ["(", "[", "{"].includes(value.text)) depth++;
        if (!value.string && [")", "]", "}"].includes(value.text)) depth--;
        if (!value.string && value.text === "," && depth === 0) break;
      }
    }
    let end = position;
    let depth = 0;
    while (end < tokens.length) {
      const value = tokens[end];
      if (!value.string && depth === 0 && [",", ")"].includes(value.text)) break;
      if (!value.string && ["(", "[", "{"].includes(value.text)) depth++;
      if (!value.string && [")", "]", "}"].includes(value.text)) depth--;
      end++;
    }
    const line = source.slice(0, token.offset).split("\n").length;
    function sourceExpression(parts) {
      while (parts[0]?.text === "&") parts = parts.slice(1);
      if (parts.length === 1 && parts[0].string) {
        const key = parts[0].text;
        options.collect?.add(key);
        if (!messages.has(key))
          errors.push(`${file}:${line}: missing source message ${JSON.stringify(key)}`);
        return true;
      }
      const marker = parts.findIndex((part) => !part.string && part.text === "msg");
      if (
        marker >= 0 &&
        parts[marker + 1]?.text === "(" &&
        parts.at(-1)?.text === ")" &&
        parts
          .slice(0, marker)
          .every((part) => !part.string && /^([a-zA-Z_][a-zA-Z0-9_]*|:)$/.test(part.text))
      )
        return sourceExpression(parts.slice(marker + 2, -1));
      if (parts[0]?.text !== "if") return false;
      let nesting = 0;
      let open = -1;
      for (let i = 1; i < parts.length; i++) {
        if (!parts[i].string && parts[i].text === "{" && nesting === 0) {
          open = i;
          break;
        }
        if (!parts[i].string && ["(", "["].includes(parts[i].text)) nesting++;
        if (!parts[i].string && [")", "]"].includes(parts[i].text)) nesting--;
      }
      if (open < 0) return false;
      let close = open + 1;
      nesting = 1;
      while (close < parts.length && nesting) {
        if (!parts[close].string && parts[close].text === "{") nesting++;
        if (!parts[close].string && parts[close].text === "}") nesting--;
        if (nesting) close++;
      }
      if (parts[close + 1]?.text !== "else") return false;
      let alternate = parts.slice(close + 2);
      if (alternate[0]?.text === "{" && alternate.at(-1)?.text === "}")
        alternate = alternate.slice(1, -1);
      const left = sourceExpression(parts.slice(open + 1, close));
      const right = sourceExpression(alternate);
      return left && right;
    }
    if (!sourceExpression(tokens.slice(position, end)) && direct)
      errors.push(
        `${file}:${line}: native source message must be a string literal or a conditional of marked source literals`,
      );
  }

  return errors;
}

async function sourceFiles(dir, rust = false) {
  const files = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    if (["locales", "test", "node_modules"].includes(entry.name)) continue;
    const file = path.join(dir, entry.name);
    if (entry.isDirectory()) files.push(...(await sourceFiles(file, rust)));
    else if (
      rust
        ? file.endsWith(".rs")
        : /\.(ts|tsx)$/.test(file) && !/\.(test|spec)\.|i18n\.tsx$|\.d\.ts$/.test(file)
    )
      files.push(file);
  }
  return files;
}

export async function checkLocales(root) {
  const catalogs = {};
  const errors = [];
  const groupNames = (
    await readdir(path.join(root, "src/locales"), { withFileTypes: true })
  )
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();
  for (const group of groups)
    if (!groupNames.includes(group)) errors.push(`${group}: missing catalog group`);
  for (const group of groupNames) {
    catalogs[group] = {};
    for (const language of languages) {
      const file = path.join(root, "src/locales", group, `${language}.json`);
      try {
        catalogs[group][language] = parseCatalog(
          await readFile(file, "utf8"),
          `${group}/${language}`,
          errors,
        );
      } catch (error) {
        errors.push(`${group}/${language}: ${error.message}`);
      }
    }
  }
  const catalogResult = checkCatalogs(catalogs, languages, groupNames);
  errors.push(...catalogResult.errors);
  const files = await sourceFiles(path.join(root, "src"));
  const configPath = ts.findConfigFile(root, ts.sys.fileExists);
  let options = {
    jsx: ts.JsxEmit.ReactJSX,
    target: ts.ScriptTarget.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler,
    module: ts.ModuleKind.ESNext,
  };
  if (configPath) {
    const config = ts.readConfigFile(configPath, ts.sys.readFile);
    options = ts.parseJsonConfigFileContent(config.config, ts.sys, root).options;
  }
  const program = ts.createProgram(files, options);
  const checker = program.getTypeChecker();
  for (const file of files)
    errors.push(
      ...checkSource(
        program.getSourceFile(file),
        path.relative(root, file),
        catalogResult.messages,
        checker,
      ),
    );
  try {
    for (const file of await sourceFiles(path.join(root, "src-tauri/src"), true))
      errors.push(
        ...checkRustSource(
          await readFile(file, "utf8"),
          path.relative(root, file),
          catalogResult.messages,
        ),
      );
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  return { errors, count: catalogResult.messages.size };
}

// Coverage gaps fall back to English at runtime, so they never break the app.
// Pull requests only warn about them; stable releases run with --complete.
// Anchored to the diagnostic's position so text inside a message key, such as
// "Error: missing token", can never turn a real error into a warning.
const coverageGap = [
  /^[^\s:/]+\/[^\s:/]+: missing translation /,
  /^[^\s:]+:\d+: missing source message /,
  /^[^\s:]+:\d+: unlocalized (UI|JSX|interpolated UI) text/,
  /^[^\s:]+:\d+: dynamic source message must come from msg\(/,
  /^[^\s:]+:\d+: native source message must be a string literal/,
];

export function isCoverageGap(message) {
  return coverageGap.some((pattern) => pattern.test(message));
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const complete = process.argv.includes("--complete");
  const result = await checkLocales(root);
  const gaps = complete ? [] : result.errors.filter(isCoverageGap);
  const errors = complete
    ? result.errors
    : result.errors.filter((error) => !isCoverageGap(error));
  if (gaps.length) {
    console.warn(gaps.join("\n"));
    console.warn(
      `${gaps.length} untranslated or unwrapped messages fall back to English. ` +
        "Fine for a pull request; they are filled in before a stable release " +
        "(npm run check:locales:complete).",
    );
  }
  if (errors.length) {
    console.error(errors.join("\n"));
    process.exitCode = 1;
  } else
    console.log(
      `Locale check passed${complete ? " (complete)" : ""}: ${languages.length} languages, ${result.count} unique source messages.`,
    );
}
