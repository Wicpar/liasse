
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
