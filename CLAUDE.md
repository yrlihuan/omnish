## About integration test
Integration tests are under tools/integration_tests. To understand how integration test works, read lib.sh and test_basic.sh.

## About build
ALWAYS do release build.

## Useful glab comments:

### view issue with comments
glab api projects/dev%2Fomnish/issues/<id>/notes

### add comment for issue
glab issue note <id> -m "评论内容"

### close issue
glab issue close <id>

### notes
when viewing issue, read both title and comments.
when closing issue, push (to get correct commit id) and append commits info.
