import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as onboarding from './airzone-onboarding.js';

test('a new gateway proposal is always read-only and never overwrites an instance', () => {
  assert.equal(typeof onboarding.gatewayProposal, 'function');
  const proposal = onboarding.gatewayProposal({ id:'airzone-2', host:'controller.example', port:'3000' }, []);
  assert.equal(proposal.error, null);
  assert.match(proposal.config, /read_only = true/);
  assert.match(proposal.config, /host = "controller.example"/);
  assert.equal(onboarding.gatewayProposal({ id:'airzone-2',host:'controller.example',port:'3000' }, [{instance:'airzone-2'}]).error, 'duplicate');
});

test('gateway proposal rejects injection, URLs, malformed addresses and ports', () => {
  assert.equal(typeof onboarding.gatewayProposal, 'function');
  for (const host of ['https://controller.example','controller.example/path','controller.example\nread_only=false','256.1.1.1','a..example','-bad.example','']) {
    assert.equal(onboarding.gatewayProposal({id:'airzone-2',host,port:'3000'}).error,'host');
  }
  for (const port of ['0','65536','1.5','3e3','']) assert.equal(onboarding.gatewayProposal({id:'airzone-2',host:'controller.example',port}).error,'port');
  assert.equal(onboarding.gatewayProposal({id:'broken"id',host:'controller.example',port:'3000'}).error,'id');
});

test('internal driver names are rejected before preparing a gateway configuration', () => {
  for (const id of ['core','assistant','automations','energie','charges','lumieres','personnes']) {
    assert.equal(onboarding.gatewayProposal({id,host:'controller.example',port:'3000'}).error,'id');
  }
});
