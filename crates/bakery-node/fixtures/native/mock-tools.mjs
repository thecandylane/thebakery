// Stateful loopback mock of the Anthropic Messages API. Drives the native
// engine through Read -> Edit -> Bash -> done. Synthetic data, loopback only.
// The engine executes the tools itself against the isolated project cwd.
// Env: MOCK_FILE (absolute path the engine will Read+Edit).
import http from 'node:http';
const F = process.env.MOCK_FILE;
let turn = 0;
function toolTurn(id, name, input) {
  return [
    ['message_start', {type:'message_start', message:{id:'msg_'+id, type:'message', role:'assistant', model:'claude-mock', content:[], stop_reason:null, usage:{input_tokens:5, output_tokens:5}}}],
    ['content_block_start', {type:'content_block_start', index:0, content_block:{type:'tool_use', id:'toolu_'+id, name, input:{}}}],
    ['content_block_delta', {type:'content_block_delta', index:0, delta:{type:'input_json_delta', partial_json: JSON.stringify(input)}}],
    ['content_block_stop', {type:'content_block_stop', index:0}],
    ['message_delta', {type:'message_delta', delta:{stop_reason:'tool_use', stop_sequence:null}, usage:{output_tokens:5}}],
    ['message_stop', {type:'message_stop'}],
  ];
}
function textDone() {
  return [
    ['message_start', {type:'message_start', message:{id:'msg_done', type:'message', role:'assistant', model:'claude-mock', content:[], stop_reason:null, usage:{input_tokens:5, output_tokens:3}}}],
    ['content_block_start', {type:'content_block_start', index:0, content_block:{type:'text', text:''}}],
    ['content_block_delta', {type:'content_block_delta', index:0, delta:{type:'text_delta', text:'DEMO_DONE'}}],
    ['content_block_stop', {type:'content_block_stop', index:0}],
    ['message_delta', {type:'message_delta', delta:{stop_reason:'end_turn', stop_sequence:null}, usage:{output_tokens:3}}],
    ['message_stop', {type:'message_stop'}],
  ];
}
function sse(res, events){ res.writeHead(200,{'content-type':'text/event-stream'}); for(const [e,d] of events) res.write(`event: ${e}\ndata: ${JSON.stringify(d)}\n\n`); res.end(); }
const server = http.createServer((req,res)=>{
  let body=''; req.on('data',c=>body+=c); req.on('end',()=>{
    if(!req.url.startsWith('/v1/messages')){ res.writeHead(200,{'content-type':'application/json'}); return res.end('{}'); }
    turn++; process.stderr.write(`MOCK turn=${turn}\n`);
    if(turn===1) return sse(res, toolTurn('read','Read',{file_path:F}));
    if(turn===2) return sse(res, toolTurn('edit','Edit',{file_path:F, old_string:'ORIGINAL', new_string:'EDITED_BY_NATIVE_ENGINE'}));
    if(turn===3) return sse(res, toolTurn('bash','Bash',{command:'echo BUILD_RAN > '+F+'.built'}));
    return sse(res, textDone());
  });
});
server.listen(0,'127.0.0.1',()=>process.stdout.write('PORT='+server.address().port+'\n'));
