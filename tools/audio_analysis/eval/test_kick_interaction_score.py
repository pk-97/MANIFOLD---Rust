"""Interaction algebra, optimiser and whole-song exclusion proofs."""
import copy
import tempfile
import unittest

import numpy as np

from .kick_interaction_score import basis, objective, FoldFitter, predict_score


def record(name, repeats=6):
    x = np.zeros((4*repeats,15))
    x[:,:2] = np.tile([[-1,-1],[-1,1],[1,-1],[1,1]],(repeats,1))
    y = (x[:,0]*x[:,1]>0).astype(int)
    return dict(track=name,features=x,labels=y,training_mask=np.ones(len(x),bool),
        event_ids=np.where(y==1,np.arange(len(x)), -1))


class InteractionTests(unittest.TestCase):
    def test_basis_scalar_and_batch(self):
        x = np.random.default_rng(1).normal(size=(7,15))*20
        actual = basis(x,np.zeros(15),np.ones(15))
        expected=[]
        for row in x:
            z=np.clip(row,-8,8); b=np.tanh(z/2)
            expected.append(list(z)+[b[i]*b[j] for i in range(15) for j in range(i,15)])
        np.testing.assert_array_equal(actual,expected)
        self.assertEqual(actual.shape,(7,135))
        self.assertLessEqual(np.abs(actual[:,15:]).max(),1)
        np.testing.assert_array_equal(actual,np.vstack([basis(row[None,:],np.zeros(15),np.ones(15)) for row in x]))

    def test_gradient(self):
        rng=np.random.default_rng(2);x=rng.normal(size=(19,135));y=rng.integers(2,size=19)
        w=rng.uniform(size=19);w/=w.sum();beta=rng.normal(size=136)*.1
        _,gradient=objective(beta,x,y,w,.01)
        for i in range(len(beta)):
            plus=beta.copy();minus=beta.copy();plus[i]+=1e-6;minus[i]-=1e-6
            numeric=(objective(plus,x,y,w,.01)[0]-objective(minus,x,y,w,.01)[0])/2e-6
            self.assertAlmostEqual(numeric,gradient[i],places=8)

    def test_conditional_signal_and_exclusion(self):
        rows=[record('a'),record('b'),record('held')]
        fitter=FoldFitter();model=fitter.fit(rows,'held',.001)
        scores=predict_score(model,rows[-1]['features'])
        self.assertTrue(np.all(scores[rows[-1]['labels']==1]>.9))
        self.assertTrue(np.all(scores[rows[-1]['labels']==0]<.1))
        changed=copy.deepcopy(rows);changed[-1]['features']*=10000;changed[-1]['labels']=1-changed[-1]['labels']
        self.assertEqual(model,fitter.fit(changed,'held',.001))
        self.assertNotIn('held',model['training_tracks'])
        self.assertAlmostEqual(model['training_weight_mass'],1)
        self.assertAlmostEqual(model['positive_weight_mass'],.5)
        self.assertLess(model['gradient_max_abs'],2e-6)

    def test_cache_and_bad_inputs(self):
        rows=[record('a'),record('b')]
        with tempfile.TemporaryDirectory() as path:
            a=FoldFitter(path).fit(rows,'b',.01)
            fitter=FoldFitter(path);b=fitter.fit(rows,'b',.01)
            self.assertEqual(a,b);self.assertEqual(fitter.disk_hits,1)
        with self.assertRaises(ValueError):basis(np.zeros((2,14)),np.zeros(15),np.ones(15))
        with self.assertRaises(ValueError):basis(np.zeros((2,15)),np.zeros(15),np.zeros(15))
        with self.assertRaises(ValueError):FoldFitter().fit(rows,'b',.2)


if __name__=='__main__':unittest.main()
