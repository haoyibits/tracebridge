// Folding a branch, and switching between the two views.
document.addEventListener('click', (event) => {
  const view = event.target.closest('.views button');
  if (view) {
    document.body.dataset.view = view.dataset.view;
    for (const button of view.parentElement.children) {
      button.setAttribute('aria-pressed', String(button === view));
    }
    return;
  }
  const button = event.target.closest('.fold');
  if (!button) return;
  const folded = button.closest('.node').classList.toggle('collapsed');
  button.textContent = folded ? '+' : '−';
  button.setAttribute('aria-expanded', String(!folded));
});
