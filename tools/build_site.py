#!/usr/bin/env python3
from __future__ import annotations

import html
import json
import re
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / 'docs-md'
OUT = ROOT / 'docs'

NAV = [('Start', [('Home', 'index.html'), ('Getting started', 'getting-started.html'), ('Mental model', 'mental-model.html')]), ('Tutorials', [('Task app', 'tutorials/tasks.html'), ('Module boundary', 'tutorials/modules.html'), ('Accounting templates', 'examples/accounting-templates.html')]), ('Features', [('Feature map', 'reference/feature-map.html'), ('Authoring format', 'features/format.html'), ('Types', 'features/types.html'), ('Collections and structs', 'features/collections.html'), ('Views', 'features/views.html'), ('Refs', 'features/refs.html'), ('Mutations', 'features/mutations.html'), ('Checks and transforms', 'features/checks-transforms.html'), ('Permissions and sessions', 'features/permissions.html'), ('Modules', 'features/modules.html'), ('Limits and sources', 'features/limits.html'), ('Blobs and storage', 'features/storage.html'), ('History and extraction', 'features/history.html'), ('Client protocol', 'features/client.html'), ('Resolved decisions', 'features/resolutions.html')]), ('Normative spec', [('README', 'spec/README.html'), ('Syntax', 'spec/SYNTAX.html'), ('Collections', 'spec/COLLECTIONS.html'), ('Mutations', 'spec/MUTATIONS.html'), ('Checks and transforms', 'spec/CHECKS-TRANSFORMS.html'), ('Modules', 'spec/MODULES.html'), ('Permissions', 'spec/PERMISSIONS.html'), ('Limits', 'spec/LIMITS.html'), ('Storage', 'spec/STORAGE.html'), ('Client', 'spec/CLIENT.html'), ('History', 'spec/HISTORY.html'), ('Resolutions', 'spec/RESOLUTIONS.html'), ('Accounting template spec', 'spec/examples/accounting-templates.html')])]


def slugify(s: str) -> str:
    s = re.sub(r'`([^`]*)`', r'\1', s)
    s = re.sub(r'<[^>]+>', '', s)
    s = re.sub(r'[^a-zA-Z0-9\s_-]+', '', s).strip().lower()
    s = re.sub(r'[\s_]+', '-', s)
    return s or 'section'


def inline_md(s: str) -> str:
    placeholders = []
    def repl_code(m):
        placeholders.append('<code>' + html.escape(m.group(1)) + '</code>')
        return f'\x00{len(placeholders)-1}\x00'
    s = re.sub(r'`([^`]+)`', repl_code, s)
    s = html.escape(s)
    s = re.sub(r'\*\*([^*]+)\*\*', r'<strong>\1</strong>', s)
    s = re.sub(r'\[([^\]]+)\]\(([^)]+)\)', lambda m: '<a href="{}">{}</a>'.format(html.escape(m.group(2)), m.group(1)), s)
    for i, val in enumerate(placeholders):
        s = s.replace(f'\x00{i}\x00', val)
    return s


def rel_href(current: str, target: str) -> str:
    cur_dir = Path(current).parent
    if str(cur_dir) == '.':
        return target
    return str(Path(*(['..'] * len(cur_dir.parts))) / target)


def md_to_html(md: str):
    lines = md.splitlines()
    out = []
    toc = []
    in_code = False
    code_lang = ''
    code_buf = []
    in_ul = False
    para = []

    def flush_para():
        nonlocal para
        if para:
            out.append('<p>' + inline_md(' '.join(x.strip() for x in para)) + '</p>')
            para = []

    def close_ul():
        nonlocal in_ul
        if in_ul:
            out.append('</ul>')
            in_ul = False

    for raw in lines:
        line = raw.rstrip('\n')
        fence = re.match(r'^```([^`]*)\s*$', line)
        if fence:
            if in_code:
                code = '\n'.join(code_buf)
                cls = ' class="language-{}"'.format(html.escape(code_lang)) if code_lang else ''
                out.append('<pre><code{}>{}</code></pre>'.format(cls, html.escape(code)))
                code_buf = []
                in_code = False
                code_lang = ''
            else:
                flush_para(); close_ul()
                in_code = True
                code_lang = fence.group(1).strip()
            continue
        if in_code:
            code_buf.append(raw)
            continue

        if not line.strip():
            flush_para(); close_ul()
            continue

        m = re.match(r'^(#{1,6})\s+(.*)$', line)
        if m:
            flush_para(); close_ul()
            level = len(m.group(1))
            text = m.group(2).strip()
            anchor = slugify(text)
            if level <= 3:
                toc.append((level, text, anchor))
            out.append(f'<h{level} id="{anchor}">{inline_md(text)}</h{level}>')
            continue

        if re.match(r'^---+$', line.strip()):
            flush_para(); close_ul(); out.append('<hr>')
            continue

        m = re.match(r'^\s*[-*]\s+(.*)$', line)
        if m:
            flush_para()
            if not in_ul:
                out.append('<ul>')
                in_ul = True
            out.append('<li>' + inline_md(m.group(1).strip()) + '</li>')
            continue

        m = re.match(r'^\s*(\d+)\.\s+(.*)$', line)
        if m:
            # Keep ordered items simple; they render as paragraphs with number for now.
            flush_para(); close_ul()
            out.append('<p class="step"><strong>{}.</strong> {}</p>'.format(m.group(1), inline_md(m.group(2).strip())))
            continue

        if line.startswith('>'):
            flush_para(); close_ul()
            out.append('<blockquote>' + inline_md(line.lstrip('> ').strip()) + '</blockquote>')
            continue

        para.append(line)

    flush_para(); close_ul()
    if in_code:
        code = '\n'.join(code_buf)
        out.append('<pre><code>{}</code></pre>'.format(html.escape(code)))
    return '\n'.join(out), toc


def page_title(md: str, fallback: str) -> str:
    for line in md.splitlines():
        if line.startswith('# '):
            return re.sub(r'[`*_]', '', line[2:].strip())
    return fallback


def nav_html(current: str) -> str:
    chunks = ['<nav class="nav">']
    chunks.append('<a class="brand" href="{}">Liasse</a>'.format(rel_href(current, 'index.html')))
    chunks.append('<input id="search" type="search" placeholder="Search docs" autocomplete="off">')
    chunks.append('<div id="search-results" class="search-results"></div>')
    for group, items in NAV:
        chunks.append('<section><h2>{}</h2>'.format(html.escape(group)))
        for label, href in items:
            cls = ' class="active"' if href == current else ''
            chunks.append('<a{} href="{}">{}</a>'.format(cls, rel_href(current, href), html.escape(label)))
        chunks.append('</section>')
    chunks.append('</nav>')
    return '\n'.join(chunks)


def toc_html(toc):
    items = [(lvl, text, anchor) for lvl, text, anchor in toc if lvl in (2,3)]
    if not items:
        return ''
    out = ['<aside class="toc"><h2>On this page</h2>']
    for lvl, text, anchor in items[:40]:
        out.append('<a class="toc-l{}" href="#{}">{}</a>'.format(lvl, anchor, html.escape(re.sub(r'`', '', text))))
    out.append('</aside>')
    return '\n'.join(out)


def wrap(current: str, title: str, body: str, toc) -> str:
    root = rel_href(current, '')
    if root == '.':
        root = ''
    css = rel_href(current, 'assets/liasse.css')
    js = rel_href(current, 'assets/search.js')
    search = rel_href(current, 'assets/search-index.json')
    return f'''<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{html.escape(title)} · Liasse</title>
  <link rel="stylesheet" href="{css}">
</head>
<body data-search-index="{search}">
  {nav_html(current)}
  <main class="content">
    <article>{body}</article>
  </main>
  {toc_html(toc)}
  <script src="{js}"></script>
</body>
</html>
'''


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    (OUT / 'assets').mkdir(parents=True)
    pages = []
    for src in sorted(SRC.rglob('*.md')):
        rel = src.relative_to(SRC)
        out_rel = rel.with_suffix('.html')
        md = src.read_text()
        body, toc = md_to_html(md)
        title = page_title(md, out_rel.stem)
        out_file = OUT / out_rel
        out_file.parent.mkdir(parents=True, exist_ok=True)
        out_file.write_text(wrap(str(out_rel).replace('\\', '/'), title, body, toc))
        text = re.sub(r'```.*?```', ' ', md, flags=re.S)
        text = re.sub(r'[#*_`>\[\]()]', ' ', text)
        pages.append({'title': title, 'href': str(out_rel).replace('\\','/'), 'text': re.sub(r'\s+', ' ', text).strip()[:3000]})

    (OUT / '.nojekyll').write_text('')
    (OUT / '404.html').write_text(wrap('404.html', 'Not found', '<h1>Not found</h1><p>The page does not exist.</p>', []))
    (OUT / 'assets' / 'search-index.json').write_text(json.dumps(pages, indent=2))
    (OUT / 'assets' / 'liasse.css').write_text(CSS)
    (OUT / 'assets' / 'search.js').write_text(JS)

CSS = r'''
:root {
  --bg: #fbfaf8;
  --panel: #ffffff;
  --text: #1f2328;
  --muted: #65707c;
  --line: #e8e2d8;
  --accent: #7047eb;
  --accent-soft: #f1edff;
  --code: #f6f1e8;
  --shadow: 0 18px 50px rgba(31, 35, 40, 0.08);
}
* { box-sizing: border-box; }
html { scroll-behavior: smooth; }
body {
  margin: 0;
  font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  background: var(--bg);
  color: var(--text);
  line-height: 1.65;
}
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
.nav {
  position: fixed;
  inset: 0 auto 0 0;
  width: 300px;
  overflow-y: auto;
  background: var(--panel);
  border-right: 1px solid var(--line);
  padding: 22px 18px 36px;
}
.brand {
  display: block;
  font-size: 1.55rem;
  font-weight: 800;
  letter-spacing: -0.04em;
  color: var(--text);
  margin-bottom: 18px;
}
.nav section { margin: 22px 0; }
.nav h2 {
  color: var(--muted);
  text-transform: uppercase;
  letter-spacing: .08em;
  font-size: .74rem;
  margin: 0 0 8px;
}
.nav a:not(.brand) {
  display: block;
  padding: 6px 9px;
  border-radius: 8px;
  color: #3f4650;
  font-size: .94rem;
}
.nav a.active, .nav a:not(.brand):hover {
  background: var(--accent-soft);
  color: var(--accent);
  text-decoration: none;
}
#search {
  width: 100%;
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 10px 12px;
  font: inherit;
  background: #fff;
}
.search-results {
  margin-top: 8px;
  border-radius: 10px;
  overflow: hidden;
  box-shadow: var(--shadow);
}
.search-results a {
  display: block;
  padding: 10px 12px;
  background: #fff;
  border-bottom: 1px solid var(--line);
  color: var(--text);
}
.search-results small { display: block; color: var(--muted); }
.content {
  margin-left: 300px;
  margin-right: 260px;
  max-width: 980px;
  padding: 48px 56px 90px;
}
article {
  background: var(--panel);
  border: 1px solid var(--line);
  border-radius: 22px;
  box-shadow: var(--shadow);
  padding: 42px 48px;
}
h1 {
  font-size: clamp(2rem, 4vw, 3.5rem);
  line-height: 1.05;
  letter-spacing: -0.06em;
  margin: 0 0 22px;
}
h2 {
  margin-top: 44px;
  font-size: 1.45rem;
  letter-spacing: -0.03em;
}
h3 { margin-top: 32px; }
p, li { color: #30363d; }
ul { padding-left: 1.25rem; }
code {
  background: var(--code);
  border-radius: 5px;
  padding: .1em .32em;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: .92em;
}
pre {
  overflow-x: auto;
  background: #161b22;
  color: #f0f6fc;
  padding: 18px 20px;
  border-radius: 14px;
  line-height: 1.5;
  box-shadow: inset 0 0 0 1px rgba(255,255,255,.08);
}
pre code { background: transparent; color: inherit; padding: 0; }
blockquote {
  margin: 24px 0;
  padding: 16px 20px;
  border-left: 4px solid var(--accent);
  background: var(--accent-soft);
  border-radius: 0 12px 12px 0;
}
.step { padding-left: 1rem; }
hr { border: 0; border-top: 1px solid var(--line); margin: 32px 0; }
.toc {
  position: fixed;
  top: 0;
  right: 0;
  width: 260px;
  height: 100vh;
  overflow-y: auto;
  padding: 56px 24px;
  color: var(--muted);
}
.toc h2 {
  margin: 0 0 12px;
  font-size: .75rem;
  text-transform: uppercase;
  letter-spacing: .08em;
  color: var(--muted);
}
.toc a {
  display: block;
  color: var(--muted);
  font-size: .88rem;
  padding: 4px 0;
}
.toc .toc-l3 { padding-left: 12px; }
@media (max-width: 1100px) {
  .toc { display: none; }
  .content { margin-right: 0; }
}
@media (max-width: 800px) {
  .nav { position: static; width: auto; max-height: none; border-right: 0; border-bottom: 1px solid var(--line); }
  .content { margin: 0; padding: 24px 16px 64px; }
  article { padding: 26px 22px; border-radius: 16px; }
}
'''

JS = r'''
(function () {
  const input = document.getElementById('search');
  const box = document.getElementById('search-results');
  if (!input || !box) return;
  const indexUrl = document.body.getAttribute('data-search-index');
  let docs = [];
  fetch(indexUrl).then(r => r.json()).then(data => { docs = data; }).catch(() => {});

  function rel(href) {
    if (href.startsWith('http')) return href;
    const index = document.body.getAttribute('data-search-index') || '';
    const prefix = index.replace(/assets\/search-index\.json$/, '');
    return prefix + href;
  }

  input.addEventListener('input', () => {
    const q = input.value.trim().toLowerCase();
    box.innerHTML = '';
    if (q.length < 2) return;
    const hits = docs.map(d => {
      const hay = (d.title + ' ' + d.text).toLowerCase();
      const score = hay.includes(q) ? (d.title.toLowerCase().includes(q) ? 2 : 1) : 0;
      return [score, d];
    }).filter(x => x[0]).sort((a,b) => b[0] - a[0]).slice(0, 8);
    for (const [, d] of hits) {
      const a = document.createElement('a');
      a.href = rel(d.href);
      a.innerHTML = '<strong>' + d.title.replace(/[&<>]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;'}[c])) + '</strong><small>' + d.href + '</small>';
      box.appendChild(a);
    }
  });
})();
'''

if __name__ == '__main__':
    main()
