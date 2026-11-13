import {after,before,test} from 'node:test';
import assert from 'node:assert/strict';
import {createServer as createHttpServer} from 'node:http';
import {fileURLToPath} from 'node:url';
import {createServer} from 'vite';

let backend,vite,preview;
const previousTarget=process.env.MOLI_URL;
before(async()=>{
  backend=createHttpServer((request,response)=>{
    response.setHeader('content-type','application/json');
    response.end(JSON.stringify({host:request.headers.host,origin:request.headers.origin,site:request.headers['sec-fetch-site']}));
  });
  await new Promise(resolve=>backend.listen(0,'127.0.0.1',resolve));
  process.env.MOLI_URL=`http://127.0.0.1:${backend.address().port}`;
  vite=await createServer({root:fileURLToPath(new URL('./',import.meta.url)),server:{host:'127.0.0.1',port:0,hmr:false},logLevel:'silent'});
  await vite.listen();
  preview=`http://127.0.0.1:${vite.httpServer.address().port}`;
});
after(async()=>{
  await vite?.close();
  backend?.closeAllConnections();
  if(backend)await new Promise(resolve=>backend.close(resolve));
  if(previousTarget===undefined)delete process.env.MOLI_URL;else process.env.MOLI_URL=previousTarget;
});
const forwarded=async(path,origin,site)=>(await fetch(preview+path,{method:'POST',headers:{origin,'sec-fetch-site':site,'content-type':'application/json'},body:'{}'})).json();

test('the development proxy preserves the browser origin/host pair for authenticated API requests',async()=>{
  const headers=await forwarded('/api/__origin_probe__',preview,'same-origin');
  assert.equal(headers.host,new URL(preview).host);
  assert.equal(headers.origin,preview);
  assert.equal(headers.site,'same-origin');
});
test('foreign and same-site origins remain unchanged for the hub to reject',async()=>{
  for(const [origin,site] of [['https://example.com','cross-site'],['http://localhost:9999','same-site'],['null','cross-site']]){
    const headers=await forwarded('/api/__origin_probe__',origin,site);
    assert.equal(headers.host,new URL(preview).host);
    assert.equal(headers.origin,origin);
    assert.equal(headers.site,site);
  }
});
test('the MCP development proxy follows the same origin-preservation rule',async()=>{
  const headers=await forwarded('/mcp',preview,'same-origin');
  assert.equal(headers.host,new URL(preview).host);
  assert.equal(headers.origin,preview);
  assert.equal(headers.site,'same-origin');
});
