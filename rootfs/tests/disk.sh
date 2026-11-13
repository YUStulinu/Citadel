# ext2 on virtio-blk: read what mke2fs put there, write new files, check them.
cat /mnt/README.txt
mkdir -p /mnt/out/sub
seq 1 20000 > /mnt/out/numbers
wc -l /mnt/out/numbers
cp /mnt/out/numbers /mnt/out/sub/copy
rm /mnt/out/numbers
ln -s sub/copy /mnt/out/link
head -n 3 /mnt/out/link
mv /mnt/out/sub /mnt/out/moved
ls -l /mnt/out /mnt/out/moved
echo "persist $(date +%s)" > /mnt/stamp
cat /proc/mounts
sync
