Install:

```
git submodule update --init --recursive

cd sqlite
./configure --enable-replication --disable-amalgamation
sudo make install
```

```
cd ..
cargo run
sqlite> .open test.db
sqlite> pragma journal_mode=wal
sqlite> <run some sql>
sqlite> .quit
```

Next:

* Register rust functions to receive WAL
* This would involve adding some new commands to shell.c
  * ```sqlite>.wal_leader```
  * ```sqlite>.wal_follower```
* Implement rust code for various callbacks from C
* Plugin one of the Raft implementation where LogEntry is a wal frame
* Run three copies of rust_wal one as leader, two as follower
* Verify correctness when leader is killed and followers take over


