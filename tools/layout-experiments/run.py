import sys, json, lay
name = sys.argv[1]
pos = lay.ALG[name]()
print(name, 'overlaps:', lay.check(pos))
json.dump(pos, open(name + '.json', 'w'))
