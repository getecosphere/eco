// Quick row structure peek
(function () {
  const table = document.querySelector('.wallet_history_table');
  if (!table) { console.log('No table found'); return; }
  const rows = table.querySelectorAll('tr');
  console.log(`Total rows: ${rows.length}`);
  // Show first 5 rows
  for (let i = 0; i < Math.min(5, rows.length); i++) {
    const cells = rows[i].querySelectorAll('td, th');
    const texts = [...cells].map(c => c.textContent.trim().substring(0, 60));
    console.log(`Row ${i} (${rows[i].className}): [${texts.join(' | ')}]`);
    console.log('  HTML:', rows[i].outerHTML.substring(0, 800));
  }
})();
