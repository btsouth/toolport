/** Keep controls and invisible Unicode visible in plain React text nodes. */
export function visibleExecutionText(text: string): string {
  return Array.from(text, (c) => {
    const n = c.codePointAt(0)!;
    return /[\p{Cc}\p{Cf}\p{Default_Ignorable_Code_Point}\u2028\u2029]/u.test(c)
      ? `\\u{${n.toString(16).toUpperCase().padStart(4, "0")}}`
      : c;
  }).join("");
}
