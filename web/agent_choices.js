/* A live question belongs to the current tool call. It cannot survive stop,
 * reconnect or a restored transcript, and choosing is never command approval. */
export function requestAgentChoice({container, question, options, multiple = false, signal, lang = 'en'}) {
  signal?.throwIfAborted();
  const zh = lang.startsWith('zh');
  const copy = zh ? {hint: multiple ? '可多选，也可以补充说明。' : '选择一项，也可以直接填写其他答案。',
    other:'其他答案或补充说明', send:'确认选择', skip:'暂不选择', answered:'已回答', cancelled:'未选择', stopped:'已取消'} :
    {hint: multiple ? 'Choose one or more options, or add your own answer.' : 'Choose an option, or write your own answer.',
    other:'Your own answer or additional details', send:'Continue', skip:'Skip', answered:'Answered', cancelled:'No answer', stopped:'Cancelled'};
  const form = document.createElement('form'); form.className = 'agent-choice';
  const fieldset = document.createElement('fieldset');
  const legend = document.createElement('legend'); legend.textContent = question;
  const hint = document.createElement('p'); hint.className = 'field-hint'; hint.textContent = copy.hint;
  const name = `agent-choice-${crypto.randomUUID()}`;
  fieldset.append(legend, hint);
  const inputs = options.map((option, index) => {
    const label = document.createElement('label'); label.className = 'agent-choice-option';
    const input = document.createElement('input'); input.type = multiple ? 'checkbox' : 'radio'; input.name = name; input.value = String(index);
    const body = document.createElement('span');
    const title = document.createElement('strong'); title.textContent = option.label; body.append(title);
    if (option.description) { const description = document.createElement('small'); description.textContent = option.description; body.append(description); }
    label.append(input, body); fieldset.append(label); return input;
  });
  const label = document.createElement('label'); label.className = 'agent-choice-other';
  const labelText = document.createElement('span'); labelText.textContent = copy.other;
  const extra = document.createElement('textarea'); extra.rows = 2; extra.maxLength = 1000; label.append(labelText, extra); fieldset.append(label);
  const actions = document.createElement('div'); actions.className = 'agent-actions';
  const send = document.createElement('button'); send.className = 'btn btn-primary'; send.type = 'submit'; send.textContent = copy.send; send.disabled = true;
  const skip = document.createElement('button'); skip.className = 'btn'; skip.type = 'button'; skip.textContent = copy.skip;
  const status = document.createElement('p'); status.className = 'field-hint'; status.setAttribute('role','status'); status.hidden = true;
  actions.append(send, skip); form.append(fieldset, actions, status); container.replaceChildren(form);
  fieldset.tabIndex = -1; fieldset.focus({preventScroll:true});
  const sync = () => { send.disabled = !inputs.some(input => input.checked) && !extra.value.trim(); };
  form.addEventListener('input', sync);
  return new Promise((resolve, reject) => {
    let settled = false;
    const finish = (answer, error) => {
      if (settled) return;
      settled = true; signal?.removeEventListener('abort', abort);
      fieldset.disabled = true; send.disabled = skip.disabled = true; actions.hidden = true;
      status.hidden = false; status.textContent = error ? copy.stopped : answer.status === 'answered' ? copy.answered : copy.cancelled;
      error ? reject(error) : resolve(answer);
    };
    const abort = () => finish(null, signal.reason || new DOMException('Aborted','AbortError'));
    form.addEventListener('submit', event => {
      event.preventDefault(); event.stopPropagation();
      const selected = inputs.flatMap((input,index) => input.checked ? [index] : []), text = extra.value.trim();
      if (selected.length || text) finish({status:'answered', selected, text});
    });
    skip.addEventListener('click', () => finish({status:'cancelled'}));
    signal?.addEventListener('abort', abort, {once:true});
    if (signal?.aborted) abort();
  });
}
