import test from 'node:test';
import assert from 'node:assert/strict';
import { setupPrompt } from '../assets/setup.js';
test('multi-PC setup honors supported allowance without transferring credentials',()=>{
 for(const limit of [1,3,5]) {
  const text=setupPrompt(limit);
  assert.match(text,new RegExp(`up to ${limit} of my computers`));
  assert.match(text,/never reuse an install link or copy device tokens/);
  assert.match(text,/Do not buy a plan/);
 }
 assert.match(setupPrompt(100),/up to 1 of my computers/);
});
