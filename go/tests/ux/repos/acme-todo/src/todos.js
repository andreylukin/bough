// A todo is { id, title, done, due } with due as an ISO date or null.
export function addTodo(list, title, due = null) {
  const id = list.length + 1; // TODO: ids collide after a delete
  return [...list, { id, title, done: false, due }];
}

export function toggle(list, id) {
  return list.map((t) => (t.id === id ? { ...t, done: !t.done } : t));
}

export function overdue(list, today) {
  // BUG: compares strings of different formats when due has a time part
  return list.filter((t) => !t.done && t.due && t.due < today);
}

export function remove(list, id) {
  return list.filter((t) => t.id !== id);
}
