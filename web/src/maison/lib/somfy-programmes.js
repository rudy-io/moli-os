// A read model only: no clock, network, mutation, or execution of provider definitions.
const LIMIT = { definitions: 512, targets: 4096, triggers: 64, actions: 100, commands: 16, text: 512 };
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const integer = (value, min, max) => Number.isSafeInteger(value) && value >= min && value <= max;
const declared = value => typeof value === 'boolean' ? value : null;
const unique = values => [...new Set(values)];
const identifier = value => typeof value === 'string' && value.length > 0 && value.length <= 2048 ? value : null;
const RULE_FIELDS = new Set(['oid', 'label', 'metadata', 'creationTime', 'lastUpdateTime', 'enabled', 'priority', 'finalRule', 'calendarDayOID', 'localCalendarDay', 'dayMask', 'formattedDayMask', 'period', 'year', 'month', 'day', 'startYear', 'startMonth', 'startDay', 'endYear', 'endMonth', 'endDay', 'startTimeHours', 'startTimeMinutes', 'endTimeHours', 'endTimeMinutes', 'presenceFilter', 'randomTimeRange']);
const TRIGGER_FIELDS = new Set(['id', 'triggerType', 'hours', 'minutes', 'dawnOffset', 'duskOffset', 'enabled', 'actionGroupOID', 'localActionGroup']);
const GROUP_FIELDS = new Set(['oid', 'label', 'actions', 'metadata', 'creationTime', 'lastUpdateTime', 'enabled', 'shortcut', 'notificationCondition', 'notificationTypeMask', 'notificationText', 'notificationTitle']);
const ACTION_FIELDS = new Set(['deviceURL', 'commands']);

function qualifyFields(value, known, warnings) {
  if (object(value) && Object.keys(value).some(key => !known.has(key))) warnings.push('conditions');
}

function text(value, warnings) {
  if (typeof value !== 'string' || !value.trim()) return null;
  if (value.length <= LIMIT.text) return value;
  warnings.push('truncated');
  return `${value.slice(0, LIMIT.text)}…`;
}

function indexDefinitions(collection, warnings) {
  const definitions = collection?.data?.definitions;
  const index = new Map();
  if (!Array.isArray(definitions)) return index;
  if (definitions.length > LIMIT.definitions) warnings.push('truncated');
  for (const definition of definitions.slice(0, LIMIT.definitions)) {
    const id = identifier(definition?.id);
    if (id) index.set(id, index.has(id) ? null : definition);
  }
  return index;
}

export function createProgrammeContext(catalog, freshnessCollections = {}) {
  const warnings = [];
  const labels = new Map();
  const targets = Array.isArray(catalog?.target_labels) ? catalog.target_labels : [];
  if (targets.length > LIMIT.targets) warnings.push('truncated');
  for (const target of targets.slice(0, LIMIT.targets)) {
    const url = identifier(target?.device_url);
    if (!url) continue;
    const label = text(target.label, warnings);
    if (!labels.has(url)) labels.set(url, label);
    else if (labels.get(url) !== label) labels.set(url, null);
  }
  return {
    scenarios: indexDefinitions(catalog?.collections?.scenarios, warnings),
    days: indexDefinitions(catalog?.collections?.calendar_days, warnings),
    labels,
    freshness: { scenarios: freshnessCollections.scenarios === true, calendar_days: freshnessCollections.calendar_days === true },
    warnings: unique(warnings),
  };
}

function dateParts(year, month, day, warnings) {
  if (year === undefined && month === undefined && day === undefined) return null;
  if (year === -1 && month === -1 && day === -1) return { kind: 'unbounded' };
  const leap = integer(year, 1, 9999) && (year % 400 === 0 || (year % 4 === 0 && year % 100 !== 0));
  const monthDays = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  if (integer(year, 1, 9999) && integer(month, 1, 12) && integer(day, 1, monthDays[month - 1])) {
    return { kind: 'date', year, month, day };
  }
  warnings.push('invalid_date');
  return { kind: 'unknown' };
}

function reference(id, index, fresh, warnings) {
  if (!identifier(id) || !index.has(id)) { warnings.push('missing_reference'); return null; }
  const found = index.get(id);
  if (!found) { warnings.push('ambiguous_reference'); return null; }
  if (!fresh) warnings.push('referenced_history');
  return found;
}

function previewParameters(parameters, warnings) {
  if (!Array.isArray(parameters)) return null;
  if (parameters.length > LIMIT.commands) { warnings.push('truncated'); return null; }
  for (const parameter of parameters) {
    if (typeof parameter === 'number' && !Number.isSafeInteger(parameter)) { warnings.push('unsafe_number'); return null; }
    if (parameter !== null && !['string', 'number', 'boolean'].includes(typeof parameter)) return null;
    if (typeof parameter === 'string' && parameter.length > 128) { warnings.push('truncated'); return null; }
  }
  const result = JSON.stringify(parameters);
  if (result.length > 256) { warnings.push('truncated'); return null; }
  return result;
}

function commandPresentation(command, warnings) {
  const name = text(command?.name, warnings);
  const parameters = command?.parameters;
  if (['open', 'close', 'up', 'down', 'on', 'off', 'stop'].includes(name) && Array.isArray(parameters) && parameters.length === 0) {
    return { kind: name, name };
  }
  if (name === 'setClosure' && Array.isArray(parameters) && parameters.length === 1 && integer(parameters[0], 0, 100)) {
    return { kind: 'set_closure', name, value: parameters[0] };
  }
  warnings.push('unknown_command');
  return { kind: 'unknown', name, parametersPreview: previewParameters(parameters, warnings) };
}

function actionsPresentation(source, context, warnings, budget) {
  if (!Array.isArray(source)) {
    warnings.push('invalid_actions');
    return { actions: [], totalActions: 0, truncated: false };
  }
  const take = Math.min(source.length, budget.actions);
  budget.actions -= take;
  let truncated = take < source.length;
  if (truncated) warnings.push('truncated');
  const actions = source.slice(0, take).map(action => {
    qualifyFields(action, ACTION_FIELDS, warnings);
    const url = identifier(action?.deviceURL);
    const label = url ? context.labels.get(url) ?? null : null;
    if (!label) warnings.push('unknown_target');
    const commands = Array.isArray(action?.commands) ? action.commands : [];
    if (!Array.isArray(action?.commands)) warnings.push('invalid_commands');
    const commandTruncated = commands.length > LIMIT.commands;
    if (commandTruncated) { warnings.push('truncated'); truncated = true; }
    return {
      target: { label, url, known: label !== null },
      commands: commands.slice(0, LIMIT.commands).map(command => commandPresentation(command, warnings)),
      totalCommands: commands.length,
      truncated: commandTruncated,
    };
  });
  return { actions, totalActions: source.length, truncated };
}

function groupPresentation(group, context, warnings, budget) {
  qualifyFields(group, GROUP_FIELDS, warnings);
  if (group.notificationTypeMask !== undefined && group.notificationTypeMask !== 0) warnings.push('conditions');
  return actionsPresentation(group.actions, context, warnings, budget);
}

function triggerTime(trigger, warnings) {
  if (trigger.triggerType === 'TIME') {
    if (integer(trigger.hours, 0, 23) && integer(trigger.minutes, 0, 59)) return { kind: 'time', hours: trigger.hours, minutes: trigger.minutes };
    warnings.push('invalid_time');
    return { kind: 'unknown' };
  }
  if (trigger.triggerType === 'DAWN' || trigger.triggerType === 'DUSK') {
    const result = { kind: trigger.triggerType === 'DAWN' ? 'dawn' : 'dusk' };
    if (['hours', 'minutes'].some(key => Object.hasOwn(trigger, key))) warnings.push('solar_time');
    const offset = trigger[result.kind === 'dawn' ? 'dawnOffset' : 'duskOffset'];
    // Modern trigger offsets are kept native until their unit and sign are attested.
    if (offset !== undefined) {
      if (Number.isSafeInteger(offset)) result.offsetNative = offset;
      else warnings.push('unsafe_number');
      if (offset !== 0) warnings.push('solar_offset');
    }
    return result;
  }
  warnings.push('invalid_time');
  return { kind: 'unknown' };
}

function triggerPresentation(wrapper, context, budget) {
  const warnings = [];
  const trigger = object(wrapper?.startActionGroupTimeTrigger) ? wrapper.startActionGroupTimeTrigger : {};
  qualifyFields(trigger, TRIGGER_FIELDS, warnings);
  const time = triggerTime(trigger, warnings);
  let group = null;
  let groupLabel = null;
  if (object(trigger.localActionGroup)) {
    group = trigger.localActionGroup;
    groupLabel = text(group.label, warnings);
    if (identifier(trigger.actionGroupOID)) warnings.push('conflicting_reference');
  } else if (trigger.localActionGroup != null) {
    warnings.push('unsupported');
  } else {
    const found = reference(trigger.actionGroupOID, context.scenarios, context.freshness.scenarios, warnings);
    if (found && object(found.raw)) {
      group = found.raw;
      groupLabel = text(found.label, warnings) ?? text(group.label, warnings);
    }
  }
  const actions = group ? groupPresentation(group, context, warnings, budget) : { actions: [], totalActions: 0, truncated: false };
  return { enabled: declared(trigger.enabled), groupEnabled: declared(group?.enabled), time, groupLabel, ...actions, warnings: unique(warnings) };
}

function appendDay(result, day, context, budget) {
  result.dayEnabled = declared(day?.enabled);
  if (!Array.isArray(day?.dayTimeTriggers)) { result.warnings.push('unsupported'); return; }
  result.totalTriggers = day.dayTimeTriggers.length;
  result.triggers = day.dayTimeTriggers.slice(0, LIMIT.triggers).map(trigger => triggerPresentation(trigger, context, budget));
  result.totalActions = result.triggers.reduce((sum, trigger) => sum + trigger.totalActions, 0);
  result.truncated ||= result.totalTriggers > result.triggers.length || result.triggers.some(trigger => trigger.truncated);
  for (const trigger of result.triggers) result.warnings.push(...trigger.warnings);
  if (result.truncated) result.warnings.push('truncated');
}

function rulePresentation(result, raw, context, budget) {
  const variants = ['weeklyCalendarRule', 'dailyCalendarRule', 'specificDayCalendarRule'].filter(key => object(raw[key]));
  if (variants.length !== 1) { result.warnings.push('unsupported'); return; }
  const variant = variants[0];
  const rule = raw[variant];
  qualifyFields(rule, RULE_FIELDS, result.warnings);
  result.enabled = declared(rule.enabled);
  result.priority = Number.isSafeInteger(rule.priority) ? rule.priority : null;
  result.finalRule = declared(rule.finalRule);
  result.dates = {
    start: dateParts(rule.startYear, rule.startMonth, rule.startDay, result.warnings),
    end: dateParts(rule.endYear, rule.endMonth, rule.endDay, result.warnings),
  };
  if (variant === 'weeklyCalendarRule') {
    if (integer(rule.dayMask, 0, 127)) result.days = { kind: 'weekdays', weekdays: [0, 1, 2, 3, 4, 5, 6].filter(day => (rule.dayMask & (1 << day)) !== 0) };
    else { result.days = { kind: 'unknown' }; result.warnings.push('invalid_days'); }
  } else if (variant === 'dailyCalendarRule') result.days = { kind: 'daily' };
  else {
    const date = dateParts(rule.year, rule.month, rule.day, result.warnings);
    if (date?.kind !== 'date') result.warnings.push('invalid_date');
    result.days = { kind: 'specific', date: date?.kind === 'date' ? date : { kind: 'unknown' } };
  }

  if (variant !== 'specificDayCalendarRule' && (variant === 'dailyCalendarRule' || rule.period !== undefined)) {
    if (integer(rule.period, 1, 10000)) {
      result.period = { kind: variant === 'dailyCalendarRule' ? 'days' : 'declared', value: rule.period };
      if (variant === 'weeklyCalendarRule' && rule.period !== 1) result.warnings.push('period');
    } else { result.period = { kind: 'unknown' }; result.warnings.push('period'); }
  }
  if ((rule.presenceFilter !== undefined && rule.presenceFilter !== 'NO_FILTER') ||
      (rule.randomTimeRange !== undefined && rule.randomTimeRange !== 0) ||
      ['startTimeHours', 'startTimeMinutes', 'endTimeHours', 'endTimeMinutes'].some(key => rule[key] !== undefined && rule[key] !== 0)) result.warnings.push('conditions');

  let selectedDay = null;
  if (object(rule.localCalendarDay)) {
    selectedDay = rule.localCalendarDay;
    if (identifier(rule.calendarDayOID)) result.warnings.push('conflicting_reference');
  } else if (rule.localCalendarDay != null) {
    result.warnings.push('unsupported');
  } else {
    selectedDay = reference(rule.calendarDayOID, context.days, context.freshness.calendar_days, result.warnings)?.raw;
  }
  if (selectedDay) appendDay(result, selectedDay, context, budget);
}

export function describeProgramme(kind, definition, context = createProgrammeContext(null)) {
  const warnings = [...context.warnings];
  const raw = object(definition?.raw) ? definition.raw : {};
  const result = {
    kind, label: text(definition?.label, warnings) ?? text(raw.label, warnings), enabled: declared(raw.enabled), dayEnabled: null,
    warnings, days: null, period: null, dates: { start: null, end: null }, priority: null, finalRule: null,
    triggers: [], actions: [], totalTriggers: 0, totalActions: 0, truncated: false,
  };
  const budget = { actions: LIMIT.actions };
  if (kind === 'scenarios') Object.assign(result, groupPresentation(raw, context, warnings, budget));
  else if (kind === 'calendar_days') appendDay(result, raw, context, budget);
  else if (kind === 'calendar_rules') rulePresentation(result, raw, context, budget);
  else warnings.push('unsupported');
  result.warnings = unique(warnings);
  return result;
}
