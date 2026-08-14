// Paste this into your browser console on https://store.steampowered.com/account/history/
// It will auto-download a steam-games.txt file

(function () {
  const items = [];

  // Select all history rows — try multiple possible selectors
  const rows = document.querySelectorAll('tr[id^="wallet_row_"], .wallet_table tr, .historyTable tr, tr[data-purchase-id]');

  rows.forEach(row => {
    // Try to find game name and price cells
    const cells = row.querySelectorAll('td');
    if (cells.length < 3) return;

    // Steam's layout: date | items | type | total
    // The "items" cell usually contains the game name(s)
    let name = '';
    let price = '';

    cells.forEach((cell, i) => {
      const text = cell.textContent.trim();
      // Price columns typically contain currency symbols
      if (text.match(/[\$\u20AC\u00A3\u00A5\u20B9Rp\p{Sc}]/u) && text.match(/\d/)) {
        price = text.replace(/\s+/g, ' ');
      }
    });

    // Name: look for the cell with game title links or text
    cells.forEach(cell => {
      const links = cell.querySelectorAll('a');
      links.forEach(link => {
        const t = link.textContent.trim();
        if (t && !t.match(/^(View|Receipt|Package|Steam|Community|Profile)/i)) {
          name = t;
        }
      });
    });

    // Fallback: pick the longest text cell that isn't price/date/type
    if (!name) {
      let longest = '';
      cells.forEach(cell => {
        const t = cell.textContent.trim();
        if (t.length > longest.length && !t.match(/^\d{1,2}\s+\w{3}/) && !t.match(/^(Purchase|Gift|In-Game|Market|Refund|Wallet)/i)) {
          longest = t;
        }
      });
      name = longest;
    }

    // Fallback price: look for any cell with a number
    if (!price) {
      cells.forEach(cell => {
        const t = cell.textContent.trim();
        if (t.match(/[\d,.]+$/) && t.length < 15) {
          price = t;
        }
      });
    }

    if (name) {
      items.push({ name: name.replace(/\s+/g, ' ').trim(), price: price.replace(/\s+/g, ' ').trim() });
    }
  });

  // Deduplicate
  const seen = new Set();
  const unique = items.filter(item => {
    const key = `${item.name}|${item.price}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });

  // Build text output
  let output = '';
  unique.forEach(item => {
    output += `${item.name}\t${item.price}\n`;
  });

  if (!output) {
    alert('No items found. Try scrolling down to load more history, then run again.');
    return;
  }

  // Auto-download as file
  const blob = new Blob([output], { type: 'text/plain' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = 'steam-games.txt';
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);

  console.log(`Downloaded ${unique.length} games to steam-games.txt`);
})();
