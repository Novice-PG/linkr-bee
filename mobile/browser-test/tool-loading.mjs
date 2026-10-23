import {groupForTool} from '../src/agent-tool-groups.mjs';

// Legacy scripted models know the action but not the new discovery turn. Model
// that turn explicitly, then require its schema before returning the action.
export function withToolGroups(respond) {
  let pending=null;
  return async request=>{
    const response=pending || await respond(request);
    const name=response.tool?.name;
    if(name && !request.tools?.some(tool=>tool.function?.name===name)) {
      if(pending)throw new Error(`load_tools did not expose ${name}`);
      const group=groupForTool(name);
      if(!group)throw new Error(`No optional group for ${name}`);
      pending=response;
      return {tool:{name:'load_tools',args:{groups:[group]}}};
    }
    pending=null;
    return response;
  };
}
