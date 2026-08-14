// Final scraper — paste on https://store.steampowered.com/account/history/
// Auto-downloads steam-games.txt with Date, Game, Total + grand total at the bottom

(function () {
  const items = [];
  const rows = document.querySelectorAll('.wallet_history_table tr.wallet_table_row');

  rows.forEach(row => {
    const dateEl = row.querySelector('.wht_date');
    const itemsEl = row.querySelector('.wht_items');
    const totalEl = row.querySelector('.wht_total');

    if (!itemsEl || !totalEl) return;

    // Extract game name: first text div in wht_items (skip .wth_payment)
    const paymentDiv = itemsEl.querySelector('.wth_payment');
    const nameDiv = itemsEl.querySelector('div:first-child');
    let name = '';
    if (paymentDiv && nameDiv) {
      // Clone and remove payment to get clean name
      const clone = itemsEl.cloneNode(true);
      const payClone = clone.querySelector('.wth_payment');
      if (payClone) payClone.remove();
      name = clone.textContent.trim().replace(/\s+/g, ' ').replace(/^Click to get help.*/, '').trim();
    } else {
      name = itemsEl.textContent.trim().replace(/\s+/g, ' ').replace(/^Click to get help.*/, '').trim();
    }

    const date = dateEl ? dateEl.textContent.trim().replace(/\s+/g, ' ') : '';
    const totalText = totalEl.textContent.trim().replace(/\s+/g, ' ');

    if (name) {
      items.push({ date, name, total: totalText });
    }
  });

  if (items.length === 0) {
    alert('No items found. Try refreshing the page.');
    return;
  }

  // Parse totals and sum
  let grandTotal = 0;
  let output = 'Date\tGame\tTotal\n';
  output += '---\t---\t---\n';

  items.forEach(item => {
    output += `${item.date}\t${item.name}\t${item.total}\n`;

    // Parse numeric value from total (handles "Rp 90 999", "$19.99", etc)
    const numeric = item.total
      .replace(/[^\d,.\-]/g, '')   // remove currency symbols and non-numeric
      .replace(/\s/g, '')          // remove spaces
      .replace(/,/g, '');          // remove thousand separators
    const val = parseFloat(numeric);
    if (!isNaN(val)) grandTotal += val;
  });

  const currencySymbol = items[0].total.replace(/[\d\s,.]+/g, '').trim();
  output += `\nTotal games: ${items.length}\n`;
  output += `Grand total: ${currencySymbol} ${grandTotal.toLocaleString()}\n`;

  // Check for next page
  const nextPage = document.querySelector('.wallet_history_pagination .pagebtn:last-child:not(.disabled)');
  if (nextPage) {
    output += '\n⚠ There may be more pages — click "Next" and run this script again for each page.\n';
  }

  // Auto-download
  const blob = new Blob([output], { type: 'text/plain' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = 'steam-games.txt';
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);

  console.log(`Downloaded ${items.length} games. Grand total: ${currencySymbol} ${grandTotal.toLocaleString()}`);
})();
