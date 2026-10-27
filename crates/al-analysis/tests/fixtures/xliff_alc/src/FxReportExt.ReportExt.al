reportextension 50100 "FX Report Ext" extends "FX Report"
{
    dataset
    {
        add(Cust)
        {
            column(CustCity; City)
            {
                Caption = 'Cust City';
            }
        }
    }

    var
        RLbl: Label 'Report ext label';
}
