// Paste this into the browser console on https://store.steampowered.com/account/history/
// It will show the page structure so I can fix the selectors

(function () {
  // Find the main content area
  const candidates = [
    document.querySelector('.wallet_table'),
    document.querySelector('.historyTable'),
    document.querySelector('[class*="history"]'),
    document.querySelector('[class*="wallet"]'),
    document.querySelector('[class*="purchase"]'),
    document.querySelector('[class*="transaction"]'),
    document.getElementById('main_contents'),
    document.querySelector('.page_content'),
    document.querySelector('#application_root'),
  ].filter(Boolean);

  console.log('=== CANDIDATE CONTAINERS ===');
  candidates.forEach((el, i) => {
    console.log(`[${i}] ${el.tagName}#${el.id || '-'}.${el.className.replace(/ /g, '.')}  (children: ${el.children.length})`);
    console.log('  HTML snippet:', el.outerHTML.substring(0, 500));
  });

  // List all tables
  console.log('\n=== ALL TABLES ===');
  document.querySelectorAll('table').forEach((t, i) => {
    const rows = t.querySelectorAll('tr');
    console.log(`[${i}] class="${t.className}" rows=${rows.length}`);
    if (rows.length <= 5 && rows.length > 0) {
      rows.forEach((r, ri) => {
        const cells = r.querySelectorAll('td, th');
        const texts = [...cells].map(c => c.textContent.trim().substring(0, 50));
        console.log(`  row ${ri}: [${texts.join(' | ')}]`);
      });
    }
  });

  // Any data-rows or list items
  console.log('\n=== ROWS WITH DATA ATTRS ===');
  document.querySelectorAll('[data-purchase-id], [data-transaction-id], [id^="wallet_row"]').forEach((el, i) => {
    console.log(`[${i}] ${el.tagName} class="${el.className}"`, el.textContent.trim().substring(0, 200));
    if (i > 10) { console.log('...(truncated)'); return; }
    // Show first row's HTML structure
    if (i === 0) console.log('  HTML:', el.outerHTML.substring(0, 600));
  });

  // Check for React-root style content
  console.log('\n=== BODY CLASSES ===');
  console.log('body class:', document.body.className);

  // Check page title
  console.log('\n=== PAGE INFO ===');
  console.log('Title:', document.title);
  console.log('URL:', location.href);

  console.log('\n\n*** Copy ALL the output above and send it to me ***');
})();
