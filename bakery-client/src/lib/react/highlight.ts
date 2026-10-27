// Syntax highlighting for the agent panel, in the operator's own colours.
//
// One lazily created shiki highlighter serves both code fences in replies
// (as Streamdown's `code` plugin — Streamdown 2 ships no highlighter of its
// own, so without this every fence rendered as plain text) and the commands,
// inputs, and outputs inside tool calls.
//
// The theme is shiki's CSS-variables theme: every token colour is a
// `--shiki-*` variable that theme.css points at the Appearance syntax
// tokens. A scheme change therefore recolours code without re-highlighting,
// and no colour originates in this file. The JavaScript regex engine keeps
// the WASM grammar engine out of the bundle; `forgiving` lets a grammar with
// a pattern it cannot compile degrade instead of throwing.

import { useEffect, useState } from "react";
import {
  bundledLanguages,
  createCssVariablesTheme,
  createHighlighter,
  createJavaScriptRegexEngine,
  type BundledLanguage,
  type Highlighter,
  type ThemedToken,
} from "shiki";
import type { CodeHighlighterPlugin } from "streamdown";

const THEME_NAME = "bakery";
const THEME = createCssVariablesTheme({ name: THEME_NAME, variablePrefix: "--shiki-", fontStyle: true });

export type Highlighted = { tokens: ThemedToken[][]; fg?: string; bg?: string };

let highlighter: Promise<Highlighter> | null = null;
const languageLoads = new Map<string, Promise<boolean>>();

function getHighlighter(): Promise<Highlighter> {
  highlighter ??= createHighlighter({
    themes: [THEME],
    langs: [],
    engine: createJavaScriptRegexEngine({ forgiving: true }),
  });
  return highlighter;
}

/** A fence name ("ts", "bash", "svelte", …) as a bundled shiki language, or
 *  null when there is nothing to highlight with. Aliases resolve because
 *  `bundledLanguages` carries them alongside the base names. */
export function resolveLanguage(lang: string | undefined): BundledLanguage | null {
  const id = (lang ?? "").trim().toLowerCase();
  if (id === "" || !(id in bundledLanguages)) return null;
  return id as BundledLanguage;
}

/** The language a file's name implies ("src/app.rs" → rust), or undefined
 *  when it implies none. Extensions double as shiki aliases for most
 *  grammars; the few whole-name files are listed. */
export function languageForPath(path: string | undefined): string | undefined {
  const name = (path ?? "").split(/[\\/]/).pop()?.toLowerCase() ?? "";
  if (name === "dockerfile") return "docker";
  if (name === "makefile") return "make";
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return undefined;
  const ext = name.slice(dot + 1);
  return resolveLanguage(ext) ?? undefined;
}

function ensureLanguage(h: Highlighter, lang: BundledLanguage): Promise<boolean> {
  let load = languageLoads.get(lang);
  if (!load) {
    load = h.loadLanguage(lang).then(
      () => true,
      () => false,
    );
    languageLoads.set(lang, load);
  }
  return load;
}

// Bounded: a streamed code fence is re-highlighted as it grows, and each
// intermediate text is a distinct key.
const CACHE_LIMIT = 400;
const cache = new Map<string, Highlighted>();

function cacheKey(code: string, lang: string): string {
  return `${lang}\u0000${code}`;
}

function remember(key: string, value: Highlighted) {
  if (cache.size >= CACHE_LIMIT) {
    const oldest = cache.keys().next();
    if (!oldest.done) cache.delete(oldest.value);
  }
  cache.set(key, value);
}

export function cachedHighlight(code: string, lang: string): Highlighted | null {
  return cache.get(cacheKey(code, lang)) ?? null;
}

// Past this, tokenizing costs more than colour is worth, and the text is
// shown plain. A tool can return megabytes.
const MAX_HIGHLIGHT_CHARS = 60_000;

/** Highlighted tokens for `code`, or null when the language is unknown, its
 *  grammar failed to load, or the text is too large — callers then show the
 *  text as it is. Carriage returns also opt out: shiki splits lines on them,
 *  and joined tokens would no longer be the exact bytes. */
export async function highlight(code: string, lang: string): Promise<Highlighted | null> {
  const resolved = resolveLanguage(lang);
  if (!resolved || code.length > MAX_HIGHLIGHT_CHARS || code.includes("\r")) return null;
  const key = cacheKey(code, lang);
  const hit = cache.get(key);
  if (hit) return hit;
  try {
    const h = await getHighlighter();
    if (!(await ensureLanguage(h, resolved))) return null;
    const { tokens, fg, bg } = h.codeToTokens(code, { lang: resolved, theme: THEME_NAME });
    const value: Highlighted = { tokens, fg, bg };
    remember(key, value);
    return value;
  } catch {
    return null;
  }
}

/** The inline style for one token: its colour variable, plus italic/bold
 *  when the grammar marks them (shiki's FontStyle bit flags). */
export function tokenStyle(token: ThemedToken): { color?: string; fontStyle?: string; fontWeight?: string } {
  const style: { color?: string; fontStyle?: string; fontWeight?: string } = {};
  if (token.color) style.color = token.color;
  const flags = token.fontStyle ?? 0;
  if (flags > 0 && flags & 1) style.fontStyle = "italic";
  if (flags > 0 && flags & 2) style.fontWeight = "600";
  return style;
}

/** React: the highlighted tokens for `code`, or null until (or unless) they
 *  exist. A cache hit renders coloured on the first frame. */
export function useHighlighted(code: string, lang: string | undefined): Highlighted | null {
  const language = lang ?? "";
  const key = cacheKey(code, language);
  const [state, setState] = useState<{ key: string; value: Highlighted | null }>(() => ({
    key,
    value: cachedHighlight(code, language),
  }));
  useEffect(() => {
    // Nothing to colour with: stay plain, without a pointless async round.
    if (resolveLanguage(language) === null) return;
    let live = true;
    const hit = cachedHighlight(code, language);
    if (hit) {
      setState({ key, value: hit });
      return;
    }
    void highlight(code, language).then((value) => {
      if (live) setState({ key, value });
    });
    return () => {
      live = false;
    };
  }, [code, language, key]);
  return state.key === key ? state.value : cachedHighlight(code, language);
}

type PluginResult = NonNullable<ReturnType<CodeHighlighterPlugin["highlight"]>>;

function toPluginResult(value: Highlighted): PluginResult {
  return {
    fg: value.fg,
    bg: value.bg,
    tokens: value.tokens.map((line) =>
      line.map((token) => {
        const { color, ...rest } = tokenStyle(token);
        return {
          content: token.content,
          offset: token.offset,
          color,
          ...(Object.keys(rest).length > 0 ? { htmlStyle: rest } : {}),
        };
      }),
    ),
  };
}

/** Streamdown's `plugins.code`: code fences in replies, highlighted with the
 *  same theme as everything else. Returns synchronously on a cache hit;
 *  otherwise Streamdown shows the plain fence and swaps in the callback's
 *  result when the grammar is ready. */
export const bakeryCodePlugin: CodeHighlighterPlugin = {
  name: "shiki",
  type: "code-highlighter",
  getSupportedLanguages: () => Object.keys(bundledLanguages),
  getThemes: () => [THEME, THEME],
  supportsLanguage: (language) => resolveLanguage(language) !== null,
  highlight({ code, language }, callback) {
    const hit = cachedHighlight(code, language);
    if (hit) return toPluginResult(hit);
    void highlight(code, language).then((value) => {
      if (value) callback?.(toPluginResult(value));
    });
    return null;
  },
};
