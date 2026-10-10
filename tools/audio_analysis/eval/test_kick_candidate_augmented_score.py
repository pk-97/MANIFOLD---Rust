"""Variable-candidate weights, empty budgets, family exclusion and runtime rows."""
import copy
import json
import unittest

import numpy as np

from .kick_candidate_augmented_score import CONDITIONS, FoldFitter, augmented_training_data, eligible_candidates, predict_score
from .kick_boosted_score import FoldFitter as OriginalFitter
from .kick_fusion_bandwise import fusion_features
from .kick_subspace_score import _training_data
from .test_kick_boosted_score import record


def group(seed=0, counts=(1,2,3,0,2)):
    rng=np.random.default_rng(seed)
    result=dict(features=[],labels=[],context_ids=[],gains=[],condition_ids=[],conditions=[])
    for context in ('c1','c2','c3','c4'):
        for (label,gain),count in zip(CONDITIONS,counts):
            cid=f'{context}:{label}:{gain:g}'
            result['conditions'].append(dict(id=cid,context=context,label=label,gain=gain,count=count))
            result['features'].extend(rng.normal(size=(count,4)).tolist())
            result['labels'].extend([label]*count);result['context_ids'].extend([context]*count)
            result['gains'].extend([gain]*count);result['condition_ids'].extend([cid]*count)
    for key in ('features','labels','context_ids','gains','condition_ids'):result[key]=np.asarray(result[key])
    result['features']=result['features'].reshape(-1,4)
    return result


class CandidateAugmentedTests(unittest.TestCase):
    def test_variable_counts_and_empty_budget_preserve_family_class_event_mass(self):
        records=[record('a'),record('b',1),record('c',2)];groups={'a':group()}
        for mass in (.1,.25,.5):
            d=augmented_training_data(records,'held',groups,mass)
            self.assertAlmostEqual(d['w'].sum(),1.)
            for family in ('a','b','c'):
                f=d['families']==family
                self.assertAlmostEqual(d['w'][f].sum(),1/3)
                for label in (0,1):self.assertAlmostEqual(d['w'][f&(d['y']==label)].sum(),1/6)
            aug=d['kinds']=='augmented'; w=d['w'][aug];g=groups['a']
            for condition in g['conditions']:
                expected=mass/3/2/4/(3 if condition['label'] else 2)
                actual=w[g['condition_ids']==condition['id']].sum()
                self.assertAlmostEqual(actual,expected if condition['count'] else 0)
            budget=d['augmentation_budgets'][0]
            self.assertAlmostEqual(budget['effective_mass'],.75*mass/3)
            self.assertAlmostEqual(budget['classes'][0]['returned'],mass/3/4)
            self.assertAlmostEqual(budget['classes'][1]['returned'],0.)
            natural=d['kinds']=='natural';_,x,y,old=_training_data(records,'held')
            for family in ('a','b','c'):
                for label in (0,1):
                    selected=(d['families'][natural]==family)&(y==label)
                    ratios=d['w'][natural][selected]/old[selected]
                    np.testing.assert_allclose(ratios,ratios[0],atol=1e-15)

    def test_all_empty_conditions_and_zero_mass_leave_natural_rows_unchanged(self):
        records=[record('a'),record('b',1)];groups={'a':group(counts=(0,0,0,0,0))}
        _,x,y,w=_training_data(records,'held')
        for mass in (0.,.1,.25,.5):
            d=augmented_training_data(records,'held',groups,mass)
            np.testing.assert_array_equal(d['x'],x);np.testing.assert_array_equal(d['y'],y);np.testing.assert_array_equal(d['w'],w)
        original=OriginalFitter().fit(records,'held',3);zero=FoldFitter(groups).fit(records,'held',0.)
        self.assertEqual(original['trees'],zero['trees'])
        for mass in (.1,.25,.5):
            d=augmented_training_data(records,'held',{'a':group()},mass)
            np.testing.assert_array_equal(d['mean'],original['mean']);np.testing.assert_array_equal(d['scale'],original['scale'])

    def test_outer_inner_families_are_excluded_before_access_or_cache_hash(self):
        records=[record('a'),record('b',1),record('outer',2),record('inner',3)]
        groups={'a':group(),'b':group(1),'outer':group(2),'inner':group(3)};f=FoldFitter(groups)
        outer=f.fit(records,'outer',.1);inner_records=[r for r in records if r['track']!='outer'];inner=f.fit(inner_records,'inner',.1)
        records[2]['features']=np.array([np.nan]);groups['outer']={}
        self.assertEqual(outer,f.fit(records,'outer',.1))
        records[3]['features']=np.array([np.nan]);groups['inner']={}
        self.assertEqual(inner,f.fit(inner_records,'inner',.1));self.assertEqual(inner['augmentation_families'],['a','b'])
        self.assertEqual(f.fits,2)

    def test_missing_catalog_and_orphan_rows_are_rejected(self):
        records=[record('a'),record('b',1)];g=group();g['conditions'].pop()
        with self.assertRaises(ValueError):augmented_training_data(records,'held',{'a':g},.1)
        g=group();g['condition_ids'][0]='orphan'
        with self.assertRaises(ValueError):augmented_training_data(records,'held',{'a':g},.1)

    def test_emission_selection_uses_runtime_rows_and_completed_prefix(self):
        sr=48000;hop=256;anchor=sr
        np.testing.assert_array_equal(eligible_candidates(np.array([185,186,187,199,200]),hop,sr,anchor),[2,3])
        t=np.arange(2*sr)/sr;x=np.zeros(len(t));active=t>=1
        age=t[active]-1;x[active]=np.exp(-age*22)*np.sin(2*np.pi*(55*age+90*(1-np.exp(-age*18))/18))
        candidates,available,features,hop=fusion_features(x,sr);selected=eligible_candidates(available,hop,sr,anchor)
        self.assertGreater(len(selected),0)
        for j in selected:
            c,a,f,h=fusion_features(x[:(available[j]+1)*hop],sr)
            k=np.flatnonzero(c==candidates[j]);self.assertEqual(len(k),1)
            np.testing.assert_allclose(f[k[0]],features[j],rtol=0,atol=1e-12)
        model=FoldFitter({'a':group()}).fit([record('a'),record('b',1)],'held',.1)
        probes=np.random.default_rng(4).normal(size=(20,4));saved=json.loads(json.dumps(model))
        np.testing.assert_array_equal(predict_score(saved,probes),[predict_score(saved,[x])[0] for x in probes])


if __name__=='__main__':unittest.main()
