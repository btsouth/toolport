#!/usr/bin/env node
// 辞書の欠落・重複・補間破損をビルド前に検出する。
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const languages = ["en", "ja", "zh-CN", "zh-TW", "ko", "es", "fr", "de", "pt-BR"];
const groups = ["common", "shell", "connections", "workflows"];
const errors = [];
const messages = new Set();
const placeholders = (text) =>
  [...new Set(text.match(/\{[a-zA-Z][a-zA-Z0-9_]*\}/g) || [])].sort().join(",");

async function catalog(group, language) {
  const file = path.join(root, "src", "locales", group, `${language}.json`);
  const source = await readFile(file, "utf8");
  // JSON.parseだけでは後勝ちになる同名キーも、構文木から検出する。
  const tree = ts.parseJsonText(file, source);
  const names = new Set();
  function visit(node) {
    if (ts.isPropertyAssignment(node)) {
      const key = node.name.text;
      if (names.has(key)) errors.push(`${group}/${language}: duplicate key ${key}`);
      names.add(key);
    }
    ts.forEachChild(node, visit);
  }
  visit(tree);
  const parsed = JSON.parse(source);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
    throw new Error(`Invalid catalog: ${file}`);
  return parsed;
}

let total = 0;
for (const group of groups) {
  const english = await catalog(group, "en");
  total += Object.keys(english).length;
  for (const [key, value] of Object.entries(english)) {
    if (!key.trim() || key !== value)
      errors.push(`${group}/en: source and value must match: ${key}`);
    messages.add(key);
  }
  for (const language of languages.slice(1)) {
    const translated = await catalog(group, language);
    for (const key of Object.keys(english)) {
      const value = translated[key];
      if (typeof value !== "string" || !value.trim())
        errors.push(`${group}/${language}: missing ${key}`);
      else if (placeholders(key) !== placeholders(value))
        errors.push(`${group}/${language}: invalid placeholders in ${key}`);
    }
    for (const key of Object.keys(translated)) {
      if (!Object.hasOwn(english, key))
        errors.push(`${group}/${language}: unknown key ${key}`);
    }
  }
}

async function sourceFiles(dir) {
  const result = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    if (entry.name === "locales" || entry.name === "test") continue;
    const file = path.join(dir, entry.name);
    if (entry.isDirectory()) result.push(...(await sourceFiles(file)));
    else if (/\.(ts|tsx)$/.test(file) && !/\.test\.|i18n\.tsx$/.test(file))
      result.push(file);
  }
  return result;
}
for (const file of await sourceFiles(path.join(root, "src"))) {
  const source = await readFile(file, "utf8");
  const tree = ts.createSourceFile(
    file,
    source,
    ts.ScriptTarget.Latest,
    true,
    file.endsWith("tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
  );
  function check(node) {
    if (!node) return;
    if (ts.isStringLiteralLike(node) && !messages.has(node.text)) {
      const line = tree.getLineAndCharacterOfPosition(node.getStart()).line + 1;
      errors.push(
        `${path.relative(root, file)}:${line}: missing source message ${node.text}`,
      );
    } else if (ts.isConditionalExpression(node)) {
      check(node.whenTrue);
      check(node.whenFalse);
    }
  }
  function visit(node) {
    if (
      ts.isCallExpression(node) &&
      ts.isIdentifier(node.expression) &&
      ["t", "tr", "translate"].includes(node.expression.text) &&
      node.arguments[0]
    )
      check(node.arguments[0]);
    if (
      ts.isJsxAttribute(node) &&
      node.name.getText(tree) === "message" &&
      node.initializer &&
      (ts.isStringLiteral(node.initializer) || ts.isJsxExpression(node.initializer))
    )
      check(
        ts.isJsxExpression(node.initializer)
          ? node.initializer.expression
          : node.initializer,
      );
    const translatedCall = (value) =>
      value &&
      ts.isCallExpression(value) &&
      ts.isIdentifier(value.expression) &&
      ["t", "tr", "translate"].includes(value.expression.text);
    if (
      ts.isBinaryExpression(node) &&
      [
        ts.SyntaxKind.EqualsEqualsEqualsToken,
        ts.SyntaxKind.ExclamationEqualsEqualsToken,
        ts.SyntaxKind.EqualsEqualsToken,
        ts.SyntaxKind.ExclamationEqualsToken,
      ].includes(node.operatorToken.kind) &&
      (translatedCall(node.left) || translatedCall(node.right))
    ) {
      const line = tree.getLineAndCharacterOfPosition(node.getStart()).line + 1;
      errors.push(
        `${path.relative(root, file)}:${line}: translate display text, not comparison values`,
      );
    }
    if (
      ts.isJsxAttribute(node) &&
      ["key", "id"].includes(node.name.getText(tree)) &&
      node.initializer &&
      ts.isJsxExpression(node.initializer) &&
      translatedCall(node.initializer.expression)
    ) {
      const line = tree.getLineAndCharacterOfPosition(node.getStart()).line + 1;
      errors.push(
        `${path.relative(root, file)}:${line}: keep element identifiers stable across languages`,
      );
    }
    ts.forEachChild(node, visit);
  }
  visit(tree);
}
if (errors.length) {
  console.error(errors.join("\n"));
  process.exitCode = 1;
} else {
  console.log(
    `Locale check passed: ${languages.length} languages, ${total} catalog entries (${messages.size} unique source messages).`,
  );
}
