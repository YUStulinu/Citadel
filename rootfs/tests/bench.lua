-- A CPU-bound Lua workload (same interpreter binary on both kernels): the kernel should not
-- matter here, which makes it a control for the system-call benchmarks.
local function fib(n) if n < 2 then return n end return fib(n - 1) + fib(n - 2) end

local function sieve(n)
  local composite, count = {}, 0
  for i = 2, n do
    if not composite[i] then
      count = count + 1
      for j = i * i, n, i do composite[j] = true end
    end
  end
  return count
end

local function strings(n)
  local t = {}
  for i = 1, n do t[#t + 1] = string.format("%d:%x", i, i * 7919) end
  local s = table.concat(t, ",")
  local hits = 0
  for _ in s:gmatch("%d+:a") do hits = hits + 1 end
  return #s + hits
end

local function time(name, f, ...)
  local best = math.huge
  local result
  for _ = 1, 3 do
    local t0 = os.clock()
    result = f(...)
    best = math.min(best, os.clock() - t0)
  end
  print(string.format("bench lua_%-10s %10.1f %10.1f %10.1f ms", name, best * 1000, best * 1000, best * 1000))
  assert(result)
end

time("fib", fib, 30)
time("sieve", sieve, 3000000)
time("strings", strings, 300000)
