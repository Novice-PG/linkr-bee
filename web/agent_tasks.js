// Task summaries only: no model settings, live tool calls, permissions or keys.
export const TASKS_KEY = 'linkr-agent-tasks-v1';
export function deviceIdentity(status) {
  if (status.targetBinding?.verified) return `target:${status.targetBinding.targetId}`;
  return status.deviceId ? JSON.stringify([status.transport, status.deviceId, status.uart || '']) : null;
}
export function redactTaskText(value) {
  return String(value || '').replace(/\b(Bearer\s+)\S+/gi, '$1[redacted]')
    .replace(/((?:password|passwd|api[_-]?key|token|secret)\s*[:=]\s*)[^\s,;]+/gi, '$1[redacted]')
    .replace(/https?:\/\/[^\s"<>]+/gi, value => {
      try { const u = new URL(value); u.username = ''; u.password = ''; u.search = ''; u.hash = ''; return u.href; } catch { return '[url]'; }
    }).slice(0, 2000);
}
export function createTaskStore(storage) {
  const read = () => {
    try {
      const data = JSON.parse(storage.getItem(TASKS_KEY) || '[]');
      return Array.isArray(data) ? data.filter(t => t && typeof t.id === 'string' && typeof t.goal === 'string' && typeof t.deviceKey === 'string').slice(-20) : [];
    } catch { return []; }
  };
  return {
    list: key => read().filter(t => t.deviceKey === key).reverse().map(t => ({ ...t, status: t.status === 'running' ? 'interrupted' : t.status, historical: true })),
    save(task) {
      if (!task.deviceKey) return;
      const item = { id: task.id, deviceKey: task.deviceKey, updatedAt: Date.now(), goal: redactTaskText(task.goal),
        summary: redactTaskText(task.summary), status: task.status,
        executions: (task.executions || []).slice(-8).map(e => ({ delivery: e.delivery, exitCode: e.exitCode,
          executionStatus: e.executionStatus, path: redactTaskText(e.download?.path), observation: e.observation })) };
      const data = read().filter(t => t.id !== item.id); data.push(item);
      storage.setItem(TASKS_KEY, JSON.stringify(data.slice(-20)));
    },
    clear(key) { storage.setItem(TASKS_KEY, JSON.stringify(read().filter(t => t.deviceKey !== key))); },
  };
}
