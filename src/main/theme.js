'use strict';
/*
 * Theme security model
 * --------------------
 * A theme is DATA, never code. Anything that is not on the allowlist below is
 * rejected (not "cleaned"), so a theme file cannot carry CSS, URLs, scripts,
 * images, fonts, HTML or paths. Validation runs in the main process on every
 * import/save, and the UI only ever applies values via CSS custom properties.
 */

const COLOR_KEYS = ['bg', 'surface', 'surface2', 'border', 'text', 'textMuted',
  'accent', 'accentText', 'success', 'warning', 'danger'];
const FONTS = ['system', 'segoe', 'mono', 'serif'];
const DENSITIES = ['compact', 'comfortable', 'spacious'];
const HEX = /^#[0-9a-fA-F]{6}$/;
const MAX_BYTES = 8 * 1024;
const FORMAT = 'downloadan-theme';
const LEGACY_FORMAT = 'fluxdm-theme';                 // theme files exported before the rename
const TOP_KEYS = new Set(['format', 'version', 'name', 'author', 'colors', 'shape', 'font']);

const isPlain = (o) => o !== null && typeof o === 'object' && !Array.isArray(o);

function cleanText(v, max) {
  if (typeof v !== 'string') return null;
  // strip control chars and bidi-override characters (used for spoofing)
  const s = v.replace(/[\u0000-\u001f\u007f\u202a-\u202e\u2066-\u2069\u200b-\u200f]/g, '').trim();
  return s.length >= 1 && s.length <= max ? s : null;
}

function lum(hex) {
  const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
    .map((v) => (v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4)));
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
}
function contrast(a, b) {
  const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

// Themes may not make the UI unreadable (prevents "invisible button" tricks).
const CONTRAST_RULES = [
  ['text', 'bg', 4.5], ['text', 'surface', 4.5], ['textMuted', 'bg', 3],
  ['textMuted', 'surface', 3], ['accentText', 'accent', 3], ['accent', 'bg', 1.5],
];

function validateTheme(input) {
  if (!isPlain(input)) return { ok: false, error: 'Theme must be a JSON object.' };
  for (const k of Object.keys(input)) {
    if (!TOP_KEYS.has(k)) return { ok: false, error: `Unknown field "${String(k).slice(0, 30)}".` };
  }
  if (input.format !== undefined && input.format !== FORMAT && input.format !== LEGACY_FORMAT) return { ok: false, error: 'Not a DownloaDan theme.' };
  if (input.version !== undefined && input.version !== 1) return { ok: false, error: 'Unsupported theme version.' };

  const name = cleanText(input.name, 40);
  if (!name) return { ok: false, error: 'Theme needs a name (1-40 characters).' };
  let author = null;
  if (input.author !== undefined && input.author !== '') {
    author = cleanText(input.author, 40);
    if (!author) return { ok: false, error: 'Author must be 1-40 characters.' };
  }

  if (!isPlain(input.colors)) return { ok: false, error: 'Missing "colors".' };
  for (const k of Object.keys(input.colors)) {
    if (!COLOR_KEYS.includes(k)) return { ok: false, error: `Unknown color "${String(k).slice(0, 30)}".` };
  }
  const colors = {};
  for (const k of COLOR_KEYS) {
    const v = input.colors[k];
    if (typeof v !== 'string' || !HEX.test(v)) return { ok: false, error: `Color "${k}" must look like #RRGGBB.` };
    colors[k] = v.toLowerCase();
  }

  const shapeIn = input.shape === undefined ? {} : input.shape;
  if (!isPlain(shapeIn)) return { ok: false, error: '"shape" must be an object.' };
  for (const k of Object.keys(shapeIn)) {
    if (k !== 'radius' && k !== 'density') return { ok: false, error: `Unknown shape field "${String(k).slice(0, 30)}".` };
  }
  const radius = shapeIn.radius === undefined ? 10 : shapeIn.radius;
  if (!Number.isInteger(radius) || radius < 0 || radius > 20) return { ok: false, error: 'Radius must be a whole number 0-20.' };
  const density = shapeIn.density === undefined ? 'comfortable' : shapeIn.density;
  if (!DENSITIES.includes(density)) return { ok: false, error: 'Invalid density.' };

  const font = input.font === undefined ? 'system' : input.font;
  if (!FONTS.includes(font)) return { ok: false, error: 'Invalid font.' };

  for (const [fg, bg, min] of CONTRAST_RULES) {
    if (contrast(colors[fg], colors[bg]) < min) {
      return { ok: false, error: `Not enough contrast between "${fg}" and "${bg}" (needs ${min}:1).` };
    }
  }
  return { ok: true, theme: { name, author, colors, shape: { radius, density }, font } };
}

// Parse a theme file's text safely. Size is checked BEFORE parsing.
function parseThemeText(text) {
  if (typeof text !== 'string' || Buffer.byteLength(text) > MAX_BYTES) {
    return { ok: false, error: 'Theme file is too large (max 8 KB).' };
  }
  let obj;
  try { obj = JSON.parse(text.replace(/^\uFEFF/, '')); } catch { return { ok: false, error: 'Not valid JSON.' }; }
  return validateTheme(obj);
}

function serializeTheme(t) {
  return JSON.stringify({
    format: FORMAT, version: 1, name: t.name, author: t.author || undefined,
    colors: t.colors, shape: t.shape, font: t.font,
  }, null, 2);
}

const BUILTIN = [
  { id: 'dark', name: 'DownloaDan Dark', author: 'DownloaDan', font: 'system', shape: { radius: 10, density: 'comfortable' },
    colors: { bg: '#0e1014', surface: '#161920', surface2: '#1e222b', border: '#2a2f3a', text: '#e7e9ee', textMuted: '#8b92a3', accent: '#4f8cff', accentText: '#ffffff', success: '#3ecf8e', warning: '#f5a524', danger: '#ff5d6c' } },
  { id: 'light', name: 'DownloaDan Light', author: 'DownloaDan', font: 'system', shape: { radius: 10, density: 'comfortable' },
    colors: { bg: '#f4f6fa', surface: '#ffffff', surface2: '#eef1f6', border: '#dde2ec', text: '#1a1d24', textMuted: '#5d6678', accent: '#2563eb', accentText: '#ffffff', success: '#16a36a', warning: '#c77700', danger: '#d93a4a' } },
  { id: 'midnight', name: 'Midnight', author: 'DownloaDan', font: 'system', shape: { radius: 14, density: 'comfortable' },
    colors: { bg: '#0a0e1a', surface: '#111733', surface2: '#182044', border: '#26305e', text: '#e6e9ff', textMuted: '#8f98c9', accent: '#7c6cff', accentText: '#ffffff', success: '#43d9a3', warning: '#ffb547', danger: '#ff6b81' } },
  { id: 'forest', name: 'Forest', author: 'DownloaDan', font: 'system', shape: { radius: 8, density: 'compact' },
    colors: { bg: '#0c1210', surface: '#131c18', surface2: '#1a2621', border: '#26362f', text: '#e4efe9', textMuted: '#85a094', accent: '#2fbf71', accentText: '#04130b', success: '#5ee0a0', warning: '#e8b64a', danger: '#f0707a' } },
];

module.exports = { COLOR_KEYS, FONTS, DENSITIES, BUILTIN, validateTheme, parseThemeText, serializeTheme, contrast };
