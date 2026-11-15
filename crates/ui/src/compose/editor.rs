//! Formatted body editor (Windows, macOS): an editable page in the system web engine where
//! bold, italics, lists, links and colours show as they are typed.
//!
//! The body's markup (`mailbox_core::compose::markup`) stays the source of truth: the page
//! starts from the markup's HTML and, on every change, sends its content back as markup
//! (`change\n<markup>`), which the compose view stores in its body field. Saving, sending,
//! signatures and quoting are unchanged. Toolbar actions become editing commands
//! ([`format_command`]); Ctrl+Enter sends `send`.
//!
//! The page runs its own script (the reading pane's web view runs none); it loads nothing from
//! the network (Content-Security-Policy) and pastes plain text only.

use mailbox_core::compose::markup;

/// Colours of the editor page (CSS).
pub struct EditorColors {
    pub background: String,
    pub text: String,
    pub placeholder: String,
    pub link: String,
    pub quote: String,
}

/// The editor page showing `body` (markup).
pub fn page(body: &str, placeholder: &str, colors: &EditorColors) -> String {
    let html = markup::to_html(&markup::parse(body));
    let EditorColors {
        background,
        text,
        placeholder: muted,
        link,
        quote,
    } = colors;
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; \
         script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:\">\
         <style>\
         html,body{{margin:0;height:100%;background:{background};color:{text};}}\
         #e{{box-sizing:border-box;min-height:100%;padding:8px 14px 24px;outline:none;\
         font:14px/1.5 {font};overflow-wrap:anywhere;}}\
         #e.empty::before{{content:attr(data-placeholder);color:{muted};pointer-events:none;\
         position:absolute;}}\
         #e a{{color:{link};}}\
         #e blockquote{{margin:0 0 0 2px!important;padding-left:10px!important;\
         border-left:3px solid {quote}!important;}}\
         #e ul,#e ol{{margin:0;padding-left:24px;}}\
         </style></head><body>\
         <div id=\"e\" contenteditable=\"true\" spellcheck=\"true\" data-placeholder=\"{ph}\">{html}</div>\
         <script>{SCRIPT}</script></body></html>",
        ph = markup::escape_html(placeholder),
        font = crate::theme::CSS_UI_FONT,
    )
}

/// The page script command for a toolbar wrap (`open` as passed to the compose view's `wrap`:
/// `**`, `*`, `__`, `~~`, `{color=#rrggbb}`, `{bg=#rrggbb}`, `{size=N}`).
pub fn format_command(open: &str) -> Option<String> {
    let command = match open {
        "**" => "mbx.cmd('bold')".to_owned(),
        "*" => "mbx.cmd('italic')".to_owned(),
        "__" => "mbx.cmd('underline')".to_owned(),
        "~~" => "mbx.cmd('strikeThrough')".to_owned(),
        _ => {
            let (key, value) = open.strip_prefix('{')?.strip_suffix('}')?.split_once('=')?;
            let is_color = value.len() == 7
                && value.starts_with('#')
                && value[1..].chars().all(|c| c.is_ascii_hexdigit());
            match key {
                "color" if is_color => format!("mbx.cmd('foreColor','{value}')"),
                "bg" if is_color => format!("mbx.cmd('hiliteColor','{value}')"),
                "size" => format!("mbx.size({})", value.parse::<u8>().ok()?),
                _ => return None,
            }
        }
    };
    Some(command)
}

/// Inserts `text` at the caret.
pub fn insert_text(text: &str) -> String {
    format!("mbx.cmd('insertText',{})", js_string(text))
}

/// Inserts markup (a signature) at the caret, formatted.
pub fn insert_markup(source: &str) -> String {
    let html = markup::to_html(&markup::parse(source));
    format!("mbx.cmd('insertHTML',{})", js_string(&html))
}

/// Links the selection to `url` (or inserts the URL as a link).
pub fn link(url: &str) -> Option<String> {
    let url = markup::safe_url(url)?;
    Some(format!("mbx.link({})", js_string(&url)))
}

/// A JavaScript string literal of `text`.
pub fn js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            '<' => out.push_str("\\u003c"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Turns the editable content into markup on every change and sends it to the app.
const SCRIPT: &str = r#"(() => {
const ed = document.getElementById('e');
document.execCommand('styleWithCSS', false, true);
document.execCommand('defaultParagraphSeparator', false, 'div');
const post = (m) => window.ipc.postMessage(m);
const NBSP = String.fromCharCode(160);

const hex = (css) => {
  if (/^#[0-9a-f]{6}$/i.test(css || '')) return css.toLowerCase();
  const m = /rgba?\((\d+),\s*(\d+),\s*(\d+)(?:,\s*([\d.]+))?\)/.exec(css || '');
  if (!m || m[4] === '0') return null;
  return '#' + [m[1], m[2], m[3]].map((n) => (+n).toString(16).padStart(2, '0')).join('');
};
const KEYWORDS = {'x-small': 10, small: 13, medium: 16, large: 18, 'x-large': 24, 'xx-large': 32, 'xxx-large': 48};
const FONT = [null, 10, 13, 16, 18, 24, 32, 48];
const px = (v) => {
  if (!v) return null;
  if (KEYWORDS[v]) return KEYWORDS[v];
  const m = /^(\d+(?:\.\d+)?)px$/.exec(v);
  return m ? Math.min(72, Math.max(6, Math.round(+m[1]))) : null;
};

function styleOf(el, base) {
  const s = Object.assign({}, base);
  const tag = el.tagName;
  if (tag === 'B' || tag === 'STRONG') s.bold = true;
  if (tag === 'I' || tag === 'EM') s.italic = true;
  if (tag === 'U') s.underline = true;
  if (tag === 'S' || tag === 'STRIKE' || tag === 'DEL') s.strike = true;
  if (tag === 'A' && el.getAttribute('href')) s.link = el.getAttribute('href');
  if (tag === 'FONT') {
    if (el.getAttribute('color')) s.color = hex(el.getAttribute('color')) || s.color;
    if (el.getAttribute('size')) s.size = FONT[+el.getAttribute('size')] || s.size;
  }
  const st = el.style;
  if (st.fontWeight) s.bold = st.fontWeight === 'bold' || +st.fontWeight >= 600;
  if (st.fontStyle) s.italic = st.fontStyle === 'italic';
  const deco = st.textDecorationLine || st.textDecoration || '';
  if (deco.includes('underline')) s.underline = true;
  if (deco.includes('line-through')) s.strike = true;
  if (st.color) s.color = hex(st.color) || s.color;
  if (st.backgroundColor) s.bg = hex(st.backgroundColor) || s.bg;
  if (st.fontSize) s.size = px(st.fontSize) || s.size;
  return s;
}

const esc = (t) => {
  let out = '';
  for (let i = 0; i < t.length; i++) {
    const c = t[i];
    if (c === '\\' || c === '*' || c === '[' || c === '{' || ((c === '_' || c === '~') && t[i + 1] === c)) out += '\\';
    out += c;
  }
  return out;
};
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

function segment(text, s) {
  const lead = text.match(/^\s*/)[0];
  const rest = text.slice(lead.length);
  const trail = rest.match(/\s*$/)[0];
  const core = rest.slice(0, rest.length - trail.length);
  if (!core) return esc(text);
  const marks = [s.bold && '**', s.underline && '__', s.strike && '~~', s.italic && '*'].filter(Boolean);
  let out = marks.join('') + esc(core) + marks.slice().reverse().join('');
  if (s.size) out = `{size=${s.size}}${out}{/}`;
  if (s.bg) out = `{bg=${s.bg}}${out}{/}`;
  if (s.color) out = `{color=${s.color}}${out}{/}`;
  if (s.link && !/[\s()]/.test(s.link)) out = `[${out}](${s.link})`;
  return esc(lead) + out + esc(trail);
}

const BLOCKS = new Set(['DIV', 'P', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'PRE', 'LI', 'SECTION', 'ARTICLE']);

function toMarkup() {
  const lines = [];
  let cur = null;
  const start = (prefix) => { cur = {prefix, segs: []}; lines.push(cur); };
  const walk = (node, s, ctx) => {
    for (const ch of node.childNodes) {
      if (ch.nodeType === 3) {
        const t = ch.data.split(NBSP).join(' ').replace(/[\r\n]+/g, ' ');
        if (!t) continue;
        if (!cur) start(ctx.quote + ctx.indent);
        const last = cur.segs[cur.segs.length - 1];
        if (last && same(last[1], s)) last[0] += t; else cur.segs.push([t, s]);
      } else if (ch.nodeType !== 1) {
        continue;
      } else if (ch.tagName === 'BR') {
        if (!cur) start(ctx.quote + ctx.indent);
        cur = null;
      } else if (ch.tagName === 'UL' || ch.tagName === 'OL') {
        cur = null;
        let n = +(ch.getAttribute('start') || 1);
        for (const li of ch.children) {
          if (li.tagName !== 'LI') continue;
          const pad = '  '.repeat(ctx.depth);
          start(ctx.quote + pad + (ch.tagName === 'OL' ? `${n++}. ` : '- '));
          walk(li, styleOf(li, s), {...ctx, depth: ctx.depth + 1, indent: pad + '  '});
          cur = null;
        }
      } else if (ch.tagName === 'BLOCKQUOTE') {
        cur = null;
        walk(ch, s, {...ctx, quote: ctx.quote + '> '});
        cur = null;
      } else if (BLOCKS.has(ch.tagName)) {
        cur = null;
        const before = lines.length;
        walk(ch, styleOf(ch, s), ctx);
        if (lines.length === before) start(ctx.quote + ctx.indent);
        cur = null;
      } else {
        walk(ch, styleOf(ch, s), ctx);
      }
    }
  };
  walk(ed, {}, {quote: '', indent: '', depth: 0});
  const out = lines.map((line) => {
    let body = line.segs.map(([t, s]) => segment(t, s)).join('');
    // Text that would read as a quote or list marker stays text (`\>`, `\-`).
    if (/^(>|- )/.test(body)) body = '\\' + body;
    return (line.prefix + body).replace(/\s+$/, '');
  });
  while (out.length && out[out.length - 1] === '') out.pop();
  return out.join('\n');
}

let timer = null;
const flush = () => { clearTimeout(timer); timer = null; post('change\n' + toMarkup()); };
const changed = () => {
  ed.classList.toggle('empty', ed.textContent === '' && !ed.querySelector('li,img'));
  clearTimeout(timer);
  timer = setTimeout(flush, 150);
};
ed.addEventListener('input', changed);
ed.addEventListener('blur', () => { if (timer) flush(); });
ed.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
    e.preventDefault();
    flush();
    post('send');
  }
});
ed.addEventListener('paste', (e) => {
  e.preventDefault();
  document.execCommand('insertText', false, e.clipboardData.getData('text/plain'));
});
ed.addEventListener('drop', (e) => e.preventDefault());
document.addEventListener('click', (e) => { if (e.target.closest('a')) e.preventDefault(); });

const inQuote = () => {
  const node = getSelection().anchorNode;
  const el = node && (node.nodeType === 1 ? node : node.parentElement);
  return !!(el && el.closest('blockquote') && ed.contains(el.closest('blockquote')));
};
window.mbx = {
  cmd(name, value) { ed.focus(); document.execCommand(name, false, value); changed(); },
  quote() { ed.focus(); document.execCommand('formatBlock', false, inQuote() ? 'div' : 'blockquote'); changed(); },
  size(n) {
    ed.focus();
    document.execCommand('fontSize', false, 7);
    for (const el of ed.querySelectorAll('font[size="7"], span[style*="xxx-large"]')) {
      const span = document.createElement('span');
      span.style.fontSize = n + 'px';
      span.append(...el.childNodes);
      el.replaceWith(span);
    }
    changed();
  },
  link(url) {
    ed.focus();
    if (getSelection().isCollapsed) {
      const a = document.createElement('a');
      a.href = url;
      a.textContent = url.replace(/^mailto:/, '');
      document.execCommand('insertHTML', false, a.outerHTML);
    } else {
      document.execCommand('createLink', false, url);
    }
    changed();
  },
  focus() {
    ed.focus();
    const range = document.createRange();
    range.selectNodeContents(ed);
    range.collapse(true);
    getSelection().removeAllRanges();
    getSelection().addRange(range);
  },
};
ed.classList.toggle('empty', ed.textContent === '' && !ed.querySelector('li,img'));
post('ready');
})();"#;

#[cfg(test)]
mod tests {
    use super::{format_command, js_string, link};

    #[test]
    fn maps_toolbar_wraps_to_commands() {
        assert_eq!(format_command("**").as_deref(), Some("mbx.cmd('bold')"));
        assert_eq!(
            format_command("~~").as_deref(),
            Some("mbx.cmd('strikeThrough')")
        );
        assert_eq!(
            format_command("{color=#ff0000}").as_deref(),
            Some("mbx.cmd('foreColor','#ff0000')")
        );
        assert_eq!(
            format_command("{bg=#00ff00}").as_deref(),
            Some("mbx.cmd('hiliteColor','#00ff00')")
        );
        assert_eq!(format_command("{size=18}").as_deref(), Some("mbx.size(18)"));
        assert_eq!(format_command("{color=red');alert(1)//}"), None);
        assert_eq!(format_command("{size=x}"), None);
    }

    #[test]
    fn script_strings_are_escaped() {
        let escaped = js_string("a\"b\\c\nd</script>");
        assert_eq!(escaped, "\"a\\\"b\\\\c\\nd\\u003c/script>\"");
        assert_eq!(link("javascript:alert(1)"), None);
        assert_eq!(
            link("example.com").as_deref(),
            Some(r#"mbx.link("https://example.com")"#)
        );
    }
}
