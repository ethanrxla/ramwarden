import test from 'node:test';
import assert from 'node:assert/strict';
import { rungForPressure, pressureSeries } from '../assets/lab.js';
test('pressure selects ordered interventions independently of RAM fullness',()=>{
  assert.deepEqual([0,1,2,5,10,15,20].map(rungForPressure),[0,0,1,2,3,4,4]);
});
test('simulated recovery changes future points, never the past',()=>{
  const baseline=pressureSeries(10),recovered=pressureSeries(10,true);
  assert.deepEqual(recovered.slice(0,7),baseline.slice(0,7));
  assert.ok(recovered.slice(7).every((v,i)=>v<baseline[i+7]));
});
