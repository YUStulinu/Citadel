# The official Lua 5.4 test suite, from the ext2 partition.
cd /mnt/lua-tests || exit 1
lua -e "_U=true" all.lua
