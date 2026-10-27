xmlport 50100 "FX XmlPort"
{
    Caption = 'FX XmlPort';

    schema
    {
        textelement(Root)
        {
            tableelement(Cust; Customer)
            {
                fieldelement(No; Cust."No.")
                {
                }
            }
        }
    }
}
