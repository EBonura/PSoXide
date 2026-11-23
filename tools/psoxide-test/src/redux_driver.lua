-- psoxide-test driver for PCSX-Redux (-cli, -softgpu, portable cfg). The harness defines PLAN and
-- OUT before this file runs. One step machine, evaluated on every vsync: hold buttons until a RAM
-- condition holds, wait until one holds, check RAM at a named checkpoint. Result lines start with
-- PTEST so the harness can read them out of the emulator's own output.
local ffi = require('ffi')
local B = PCSX.CONSTS.PAD.BUTTON
local BUTTON = {cross=B.CROSS, circle=B.CIRCLE, square=B.SQUARE, triangle=B.TRIANGLE,
  start=B.START, select=B.SELECT, up=B.UP, down=B.DOWN, left=B.LEFT, right=B.RIGHT,
  l1=B.L1, r1=B.R1, l2=B.L2, r2=B.R2, l3=B.L3, r3=B.R3}

local function say(...) print('PTEST', ...); io.stdout:flush() end

-- In -cli the pad poll asks ImGui about keyboard bindings and there is no ImGui context:
-- unbind every key first (it crashed every disc ~20 frames after boot otherwise).
pcall(function()
  local p = PCSX.settings.pads[1]
  for _, k in ipairs{'Up','Right','Down','Left','Cross','Triangle','Square','Circle','Select','Sstart',
                     'L1','L2','L3','R1','R2','R3'} do p['Keyboard_Pad'..k] = 0 end
  p.Keyboard_AnalogMode = 0
end)
-- No audio to the speakers: set Mute on every settings table that has it.
pcall(function()
  for _, t in pairs(PCSX.settings) do
    if type(t) == 'table' or type(t) == 'userdata' then pcall(function() t.Mute = true end) end
  end
end)
PCSX.settings.pads[1].DeviceType = PLAN.device
local pad = PCSX.SIO0.slots[1].pads[1]
pad.map()

local mem = ffi.cast('const uint8_t*', PCSX.getMemPtr())
local function read(addr, size, signed)
  local a = bit.band(addr, 0x1fffff)
  local v = mem[a]
  if size >= 2 then v = v + mem[a + 1] * 256 end
  if size == 4 then v = v + mem[a + 2] * 65536 + mem[a + 3] * 16777216 end
  if signed then
    local top = 2 ^ (size * 8)
    if v >= top / 2 then v = v - top end
  end
  return v
end
local function holds(p)
  local v = read(p.addr, p.size, p.signed)
  if p.op == 'eq' then return v == p.value
  elseif p.op == 'ne' then return v ~= p.value
  elseif p.op == 'lt' then return v < p.value
  elseif p.op == 'le' then return v <= p.value
  elseif p.op == 'gt' then return v > p.value
  elseif p.op == 'ge' then return v >= p.value
  elseif p.op == 'between' then return v >= p.value and v <= p.value_hi
  elseif p.op == 'zero' then return v == 0
  elseif p.op == 'nonzero' then return v ~= 0 end
  return false
end
local function all(step)
  for _, p in ipairs(step.probes) do
    if not holds(p) then
      return false, string.format('%s (read %s)', p.text, tostring(read(p.addr, p.size, p.signed)))
    end
  end
  return true
end

local function shot(name)
  pcall(function()
    local s = PCSX.GPU.takeScreenShot()
    if s.width == 0 then return end
    local px = ffi.cast('const uint16_t*', s.data.data)
    local f = io.open(OUT .. '/' .. name .. '.ppm', 'wb')
    f:write(string.format('P6 %d %d 255\n', s.width, s.height))
    local t = {}
    for i = 0, s.width * s.height - 1 do
      local v = px[i]
      t[#t + 1] = string.char(bit.lshift(bit.band(v, 31), 3), bit.lshift(bit.band(bit.rshift(v, 5), 31), 3),
        bit.lshift(bit.band(bit.rshift(v, 10), 31), 3))
      if #t >= 4096 then f:write(table.concat(t)); t = {} end
    end
    f:write(table.concat(t)); f:close()
  end)
end

local frame, distinct, last = 0, 0, nil
local index, waited, held, finished, idle = 1, 0, {}, false, 0
local function release()
  for b, _ in pairs(held) do pad.clearOverride(b) end
  held = {}
end
local function finish(failure)
  if finished then return end
  finished = true
  release()
  if failure then say('FAIL', failure) end
  say('DONE', frame, distinct)
  PCSX.quit(0)
end

-- Advance through every step that can complete on this vsync.
local function advance()
  while index <= #PLAN.steps do
    local step = PLAN.steps[index]
    if step.kind == 'check' then
      shot(step.name)
      local ok, bad = all(step)
      if not ok then return finish(string.format('checkpoint %s at frame %d: %s does not hold', step.name, frame, bad)) end
      say('REACHED', step.name, frame, distinct)
      index = index + 1
    else
      local ok, bad = all(step)
      if ok then
        release(); index = index + 1; waited = 0
      elseif waited >= step.timeout or frame >= PLAN.max_frames then
        return finish(string.format('%s: %s still false after %d frames (frame %d)', step.label, bad, waited, frame))
      else
        local want = {}
        for _, b in ipairs(step.buttons or {}) do want[BUTTON[b]] = true end
        for b, _ in pairs(held) do if not want[b] then pad.clearOverride(b); held[b] = nil end end
        for b, _ in pairs(want) do if not held[b] then pad.setOverride(b); held[b] = true end end
        waited = waited + 1
        return
      end
    end
  end
  if idle < (PLAN.idle_frames or 0) then idle = idle + 1; return end
  finish(nil)
end

local function hash(d)
  local p = ffi.cast('const uint16_t*', d.data)
  local h = 2166136261
  for i = 0, (#d / 2) - 1, 3 do h = bit.band(bit.bxor(h, p[i]) * 16777619, 0x7fffffff) end
  return h
end

DRIVER = PCSX.Events.createEventListener('GPU::Vsync', function()
  if finished then return end
  frame = frame + 1
  local ok = pcall(function()
    local s = PCSX.GPU.takeScreenShot()
    if s.width > 0 then
      local h = hash(s.data)
      if h ~= last then distinct = distinct + 1; last = h end
    end
  end)
  if not ok and frame == 1 then say('WARN', 'no screenshot API: distinct pictures not counted') end
  advance()
end)
PCSX.resumeEmulator()
