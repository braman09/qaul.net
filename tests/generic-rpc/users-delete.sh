#!/bin/bash

# returns a list of all users
#
# usage:
# ./users_list.sh

curl -i  \
    -H "Content-Type: application/json" \
    -d "{ \"id\": \"1\", 
          \"kind\": \"users\", 
          \"method\": \"delete\",
          \"data\": {
            \"purge\": true
          },
          \"auth\": {
            \"id\":\"$QAUL_ID\",
            \"token\":\"$QAUL_TOKEN\"
          }
        }" \
    "http://127.0.0.1:9900/rpc"
