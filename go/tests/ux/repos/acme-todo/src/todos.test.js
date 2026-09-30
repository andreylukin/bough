import { test } from 'node:test';
import assert from 'node:assert/strict';
import { addTodo, toggle, remove, overdue } from './todos.js';

test('add then toggle', () => {
  const l = toggle(addTodo([], 'milk'), 1);
  assert.equal(l[0].done, true);
});

test('ids stay unique after a delete', () => {
  let l = addTodo(addTodo([], 'a'), 'b');
  l = addTodo(remove(l, 1), 'c');
  assert.notEqual(l[0].id, l[1].id);
});

test('overdue ignores done items', () => {
  const l = toggle(addTodo([], 'x', '2026-01-01'), 1);
  assert.deepEqual(overdue(l, '2026-02-01'), []);
});
